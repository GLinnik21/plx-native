//! FFmpeg FFI (libavformat / libavcodec / libavutil). The TV ships FFmpeg n3.3
//! (SONAMEs libavformat.so.57 / libavcodec.so.57 / libavutil.so.55 = 57.71.100 /
//! 57.89.100 / 55.58.100); we link stub `.so`s carrying those SONAMEs and the device
//! loads the real libraries at runtime — the same stub trick as SDL/GLES/Starfish.
//!
//! This module is THE media demuxer: robust MKV/MP4/TS demux, HTTP input, and
//! index-based seeking (av_seek_frame). Design record: docs/ffmpeg-demuxer-plan.md.
//! It also owns the ONE encode path (`venc`, near the end of the file): the dev
//! capture stream's MPEG1 + MPEG-TS muxer, kept here because every FFmpeg ABI
//! detail — struct offsets, AVOption names, custom AVIO — belongs in one module.
//! The struct layouts below are the FFmpeg n3.3 ABI, confirmed on-device by the
//! Phase A probe (/tmp/plxnative-ffprobe logs codec_id/width/height for a known title).
#![allow(dead_code)]
use crate::aq::AuQueue;
use crate::player::threads::SendPtr;
use crate::player::SHARED;
use crate::stream::HttpStream;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uchar, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

/// Feed audio (es=2) to the pipeline. Cleared by the /tmp/plxnative-noaudio dev trigger to
/// A/B whether the audio ES (E-AC3/Atmos) is what stalls the sink on 4K HEVC.
static FEED_AUDIO: AtomicBool = AtomicBool::new(true);
/// Does the FFmpeg on this device have the ABI these offsets were written for? Set once by
/// [`boot`]; read by [`demux`]. Starts FALSE so the gate is closed until boot has actually looked
/// — a demux that somehow ran first should refuse, not assume.
static ABI_OK: AtomicBool = AtomicBool::new(false);
pub(crate) fn set_feed_audio(on: bool) {
    FEED_AUDIO.store(on, Ordering::Relaxed);
}

// ---- opaque handles (pointer-only, never dereferenced) ----
pub enum AVClass {}
pub enum AVInputFormat {}
pub enum AVIOContext {}
pub enum AVDictionary {}
pub enum AVCodec {}
pub enum AVCodecContext {} // opaque: we only pass the lib-allocated pointer to decode/free
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

// AVSubtitle (avcodec.h) — one decoded subtitle "display set". sizeof 32 on 32-bit ARM
// (u16 format @0; u32 start/end_display_time @4/@8, ms relative to `pts`; num_rects @12;
// rects ptr @16; i64 pts @24 in AV_TIME_BASE µs). repr(C) inserts the +20 pad before pts.
#[repr(C)]
pub struct AVSubtitle {
    pub format: u16,                       // +0  (0 = graphics/bitmap)
    pub start_display_time: u32,           // +4
    pub end_display_time: u32,             // +8
    pub num_rects: c_uint,                 // +12
    pub rects: *mut *mut AVSubtitleRect,   // +16
    pub pts: i64,                          // +24
}

// AVSubtitleRect (avcodec.h, libavcodec 57 / FFmpeg 3.x). CRUCIAL ABI detail: with
// FF_API_AVPICTURE (major < 58, i.e. this build) a DEPRECATED `AVPicture pict` is embedded
// after nb_colors — 8 data ptrs + 8 linesizes (64 bytes) — which shifts the live `data[4]`
// /`linesize[4]` down by 64. The Pass-A probe logs both the `data[]` and the `pict_data[]`
// slots to confirm which the decoder populates before Pass B reads pixels. sizeof = 132.
#[repr(C)]
pub struct AVSubtitleRect {
    pub x: c_int,                  // +0
    pub y: c_int,                  // +4
    pub w: c_int,                  // +8
    pub h: c_int,                  // +12
    pub nb_colors: c_int,          // +16
    pub pict_data: [*mut u8; 8],   // +20  (AVPicture.data — deprecated, FF_API_AVPICTURE)
    pub pict_linesize: [c_int; 8], // +52  (AVPicture.linesize — deprecated)
    pub data: [*mut u8; 4],        // +84  data[0]=PAL8 indices, data[1]=palette (256×BGRA)
    pub linesize: [c_int; 4],      // +100
    pub type_: c_int,              // +116 (enum AVSubtitleType: 0=NONE, 1=BITMAP, 2=TEXT, 3=ASS)
    pub text: *mut c_char,         // +120
    pub ass: *mut c_char,          // +124
    pub flags: c_int,              // +128
}

// ---- externs, bound at RUNTIME by SONAME candidate list (see `dynlib`) ----
//
// These were four `#[link]` directives, which put `libavformat.so.57` and its three siblings in
// DT_NEEDED. That is fatal on any firmware that moved the major: webOS 5.3.1 ships 58/58/56/5,
// 10.2.0 ships 59/59/57/6, 11.2.0 ships 60/60/58/7 — and a missing DT_NEEDED kills the process at
// exec(), before main, before this log file is even open. DT_NEEDED also cannot say "57 OR 58", so
// one binary spanning both eras has to do the resolution itself.
//
// The candidate lists reach past the majors this table describes ON PURPOSE. Opening a
// libavformat 59 does not make the offsets below correct — `boot()` still refuses it — but the
// difference between refusing and never starting is the difference between a UI that browses your
// library and says it cannot play, and a television where nothing happens at all.
//
// Two consequences worth knowing. The `cfg(test)` gate is gone: with no link directive the host
// suite links unconditionally, and a test that calls into FFmpeg now fails by taking `dlopen`'s
// None branch on Darwin instead of by failing to link. And `av_register_all` is the one symbol
// that ever disappears (deleted in libavformat 59), so on 10.2.0+ the load reports Incomplete and
// names it — which is a correct refusal, not a bug to work around, until someone with that
// hardware makes the call optional.
crate::dynlib! {
    avformat: ["libavformat.so.57", "libavformat.so.58", "libavformat.so.59", "libavformat.so.60"] {
    fn av_register_all();
    // Declared beside libavformat's other entry points because that is the library that DEFINES
    // it. Under `#[link]` the final link resolved every name against every library at once and
    // the grouping was cosmetic; `dlsym` searches one handle and its dependency chain, and
    // libavutil does not depend on libavformat — so leaving this in the avutil block reported the
    // whole avutil table as Incomplete on every device, including working ones.
    fn avformat_version() -> c_uint;
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
    // MPEG-TS mux (dev capture stream, venc section)
    fn avformat_alloc_output_context2(
        ctx: *mut *mut AVFormatContext,
        oformat: *mut c_void,
        format_name: *const c_char,
        filename: *const c_char,
    ) -> c_int;
    fn avformat_new_stream(s: *mut AVFormatContext, c: *const AVCodec) -> *mut AVStream;
    fn avformat_write_header(s: *mut AVFormatContext, options: *mut *mut AVDictionary) -> c_int;
    fn av_write_frame(s: *mut AVFormatContext, pkt: *mut AVPacket) -> c_int;
    fn avio_flush(s: *mut AVIOContext);
    fn avformat_free_context(s: *mut AVFormatContext);
}}
crate::dynlib! {
    avcodec: ["libavcodec.so.57", "libavcodec.so.58", "libavcodec.so.59", "libavcodec.so.60"] {
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
    // Image-subtitle software decode (PGS/VobSub/DVB → paletted bitmap). The classic 3.x
    // API: decode_subtitle2 fills an AVSubtitle the caller must avsubtitle_free.
    fn avcodec_find_decoder(id: c_int) -> *const AVCodec;
    fn avcodec_alloc_context3(codec: *const AVCodec) -> *mut AVCodecContext;
    fn avcodec_parameters_to_context(ctx: *mut AVCodecContext, par: *const AVCodecParameters) -> c_int;
    fn avcodec_open2(ctx: *mut AVCodecContext, codec: *const AVCodec, opts: *mut *mut AVDictionary) -> c_int;
    fn avcodec_decode_subtitle2(
        ctx: *mut AVCodecContext,
        sub: *mut AVSubtitle,
        got_sub: *mut c_int,
        pkt: *mut AVPacket,
    ) -> c_int;
    fn avsubtitle_free(sub: *mut AVSubtitle);
    fn avcodec_free_context(ctx: *mut *mut AVCodecContext);
    // Scratch AVCodecParameters for `sub_canvas` — library-allocated so its true size (136 on
    // this build) is never our problem, and `from_context` starts by av_freep-ing par->extradata.
    // Both PRESENT on the device's own libavcodec.so.57.89.100 (`tools/abi-probe.sh has
    // libavcodec.so.57 avcodec_parameters_alloc avcodec_parameters_free`, 2026-07-29) — the stub
    // .so links either way, so this is checked, not assumed.
    fn avcodec_parameters_alloc() -> *mut AVCodecParameters;
    fn avcodec_parameters_free(par: *mut *mut AVCodecParameters);
    fn avcodec_find_encoder_by_name(name: *const c_char) -> *const AVCodec;
    // MPEG1 encode (dev capture stream, venc section)
    fn avcodec_send_frame(ctx: *mut AVCodecContext, frame: *const c_void) -> c_int;
    fn avcodec_receive_packet(ctx: *mut AVCodecContext, pkt: *mut AVPacket) -> c_int;
    fn avcodec_parameters_from_context(par: *mut AVCodecParameters, ctx: *const AVCodecContext) -> c_int;
    fn av_packet_rescale_ts(pkt: *mut AVPacket, tb_src: AVRational, tb_dst: AVRational);
}}
crate::dynlib! {
    // avformat_version lives in libavformat, not libavutil — but it was declared here and the
    // loader resolves by symbol, not by header, so it must move to the library that defines it or
    // the whole avutil table reports Incomplete on every device.
    avutil: ["libavutil.so.55", "libavutil.so.56", "libavutil.so.57", "libavutil.so.58"] {
    fn avutil_version() -> c_uint;
    fn av_malloc(size: usize) -> *mut c_void;
    fn av_freep(ptr: *mut c_void);
    fn av_rescale_q(a: i64, bq: AVRational, cq: AVRational) -> i64;
    // MPEG1 encode input frames + option/pixfmt helpers (venc section)
    fn av_frame_alloc() -> *mut c_void;
    fn av_frame_free(frame: *mut *mut c_void);
    fn av_frame_get_buffer(frame: *mut c_void, align: c_int) -> c_int;
    fn av_frame_make_writable(frame: *mut c_void) -> c_int;
    fn av_opt_set(obj: *mut c_void, name: *const c_char, val: *const c_char, search_flags: c_int) -> c_int;
    fn av_get_pix_fmt(name: *const c_char) -> c_int;
}}
crate::dynlib! {
    swscale: ["libswscale.so.4", "libswscale.so.5", "libswscale.so.6", "libswscale.so.7"] {
    fn sws_getContext(
        src_w: c_int, src_h: c_int, src_fmt: c_int,
        dst_w: c_int, dst_h: c_int, dst_fmt: c_int,
        flags: c_int, src_filter: *mut c_void, dst_filter: *mut c_void, param: *const f64,
    ) -> *mut c_void;
    fn sws_scale(
        c: *mut c_void,
        src_slice: *const *const u8, src_stride: *const c_int,
        src_slice_y: c_int, src_slice_h: c_int,
        dst: *const *mut u8, dst_stride: *const c_int,
    ) -> c_int;
    fn sws_freeContext(c: *mut c_void);
}}

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

// ================================ venc =====================================
// Dev capture stream MPEG1/TS encoder (used by capture.rs when a client's hello
// asks for kind=mpegts): RGBA 960x540 -> sws_scale -> yuv420p AVFrame ->
// mpeg1video (the TV build keeps this encoder) -> mpegts muxer -> custom AVIO
// write callback -> the client socket, raw unframed TS (self-syncing 188B pkts).
//
// ABI strategy (workflow-verified 2026-07-24 against the DEVICE's own libs — the
// AVOption tables inside libavcodec.so.57.89.100 carry the true field offsets):
// every numeric encoder knob goes through av_opt_set ("b"/"g"/"bf"/"maxrate"/
// "bufsize"/"dct"/"time_base"), pix fmts come from av_get_pix_fmt() at runtime
// (AV_PIX_FMT_RGBA is 28 in THIS build — an FF_API_XVMC alias shifts the enum;
// never hardcode it), and only width/height/pix_fmt (AVCodecContext) and
// data/linesize/width/height/format/pts (AVFrame) are raw offset pokes below.
// After avcodec_open2, avcodec_parameters_from_context round-trips the values
// through the already-modeled AVCodecParameters as a runtime ABI self-check —
// a mismatch latches the whole mpeg path off instead of corrupting memory.
// mpeg1video only accepts the fixed MPEG1 frame-rate table: time_base stays
// {1,30} regardless of the actual (jittery, <=30fps) capture cadence; jsmpeg in
// streaming mode ignores timestamps and decodes as data arrives.

// AVCodecContext (n3.3, 32-bit ARM). These three are POKED rather than set through AVOptions,
// and the note that used to sit here — "width/height/pix_fmt have no AVOption" — was wrong.
// `video_size` (an IMAGE_SIZE option writing width AND height) and `pixel_format` are both in
// libavcodec's own option table at these very offsets, verified against the DEVICE's binary and
// present identically in 4.x. Venc::open now sets them by name; these constants remain only
// because the venc self-check reads them back.
const OFF_CTX_WIDTH: usize = 124;
const OFF_CTX_HEIGHT: usize = 128;
const OFF_CTX_PIX_FMT: usize = 144;
// AVFrame (avutil 55, 32-bit ARM). pts sits at +104: a 4-byte pad at +100
// 8-aligns the int64 on ARM EABI (the classic AVFrame-on-ARM quirk).
const OFF_FRAME_DATA: usize = 0; // u8*[8]
const OFF_FRAME_LINESIZE: usize = 32; // c_int[8]
const OFF_FRAME_WIDTH: usize = 68;
const OFF_FRAME_HEIGHT: usize = 72;
const OFF_FRAME_FORMAT: usize = 80;
const OFF_FRAME_PTS: usize = 104;
const SWS_BILINEAR: c_int = 2;
const VENC_TB: AVRational = AVRational { num: 1, den: 30 };

#[inline]
unsafe fn poke_i32(base: *mut c_void, off: usize, v: i32) {
    *((base as *mut u8).add(off) as *mut i32) = v;
}
#[inline]
unsafe fn poke_i64(base: *mut c_void, off: usize, v: i64) {
    *((base as *mut u8).add(off) as *mut i64) = v;
}

/// The AVIO write side: raw TS bytes -> the mpeg client's socket. Boxed so the
/// opaque pointer stays stable; `fd` is refreshed by capture.rs before each
/// encode, `failed` reports a dead socket back without panicking inside FFmpeg.
struct VencSink {
    fd: c_int,
    failed: bool,
}

extern "C" fn venc_write_cb(op: *mut c_void, data: *mut u8, n: c_int) -> c_int {
    unsafe {
        let s = &mut *(op as *mut VencSink);
        if s.failed || s.fd < 0 || n <= 0 {
            s.failed = true;
            return -1;
        }
        // one socket-write policy for the whole feature (partial writes, MSG_NOSIGNAL)
        if !crate::capture::send_all(s.fd, std::slice::from_raw_parts(data, n as usize)) {
            s.failed = true;
            return -1;
        }
        n
    }
}

pub(crate) struct Venc {
    ctx: *mut AVCodecContext,
    frame: *mut c_void,
    sws: *mut c_void,
    oc: *mut AVFormatContext,
    st: *mut AVStream,
    avio: *mut AVIOContext,
    pkt: *mut AVPacket,
    sink: Box<VencSink>,
    st_tb: AVRational,
    pts: i64,
    pub(crate) w: c_int,
    pub(crate) h: c_int,
    // RGBA -> YUV420P directly runs swscale's GENERAL scaler — scalar C on this ARM
    // build, measured ~80ms/frame at 960x540. RGBA -> NV12 has a NEON unscaled
    // converter (rgbx_to_nv12_neon), so we go RGBA -NEON-> NV12 (Y lands straight in
    // the frame's Y plane, interleaved UV in this scratch) and deinterleave UV into
    // the planar U/V planes ourselves (~260KB pass) — mpeg1video only eats yuv420p.
    nv12_uv: Vec<u8>,
    // rolling perf split, logged every ~5s: is the cost sws (colorspace) or the codec?
    t_sws_us: u64,
    t_enc_us: u64,
    t_n: u32,
    t_last_log: std::time::Instant,
}
// All FFmpeg objects are owned and touched by the single capture-encoder thread.
unsafe impl Send for Venc {}

impl Venc {
    /// Bring up encoder + muxer for one client session. Any failure logs and
    /// returns None (capture latches the mpeg path off for that client).
    /// `w`/`h` follow whatever the shared GL capture chain is producing (960x540 or
    /// 480x270) — the encoder is NOT pinned to one size. Cost scales with macroblock
    /// count and MPEG1 has no intra prediction, so a detailed screen at 960x540 can cost
    /// 50-110ms/frame on this CPU where 480x270 stays near 15-25ms; the caller rebuilds
    /// the session when the geometry changes.
    pub(crate) fn open(w: c_int, h: c_int, bitrate_bps: i64) -> Option<Box<Venc>> {
        ensure_registered(); // the file's ONE registration guard (av_register_all + network_init)
        unsafe {
            let cname = b"mpeg1video\0".as_ptr() as *const c_char;
            let codec = avcodec_find_encoder_by_name(cname);
            if codec.is_null() {
                crate::log("venc: mpeg1video encoder absent");
                return None;
            }
            let fmt_yuv = av_get_pix_fmt(b"yuv420p\0".as_ptr() as *const c_char);
            let fmt_rgba = av_get_pix_fmt(b"rgba\0".as_ptr() as *const c_char);
            let fmt_nv12 = av_get_pix_fmt(b"nv12\0".as_ptr() as *const c_char);
            if fmt_yuv < 0 || fmt_rgba < 0 || fmt_nv12 < 0 {
                crate::log("venc: pix fmt lookup failed");
                return None;
            }
            let ctx = avcodec_alloc_context3(codec);
            if ctx.is_null() {
                return None;
            }
            let mut v = Box::new(Venc {
                ctx,
                frame: std::ptr::null_mut(),
                sws: std::ptr::null_mut(),
                oc: std::ptr::null_mut(),
                st: std::ptr::null_mut(),
                avio: std::ptr::null_mut(),
                pkt: std::ptr::null_mut(),
                sink: Box::new(VencSink { fd: -1, failed: false }),
                st_tb: AVRational { num: 1, den: 90000 },
                pts: 0,
                w,
                h,
                nv12_uv: vec![0u8; (w * h / 2) as usize],
                t_sws_us: 0,
                t_enc_us: 0,
                t_n: 0,
                t_last_log: std::time::Instant::now(),
            });
            // {1,30} is in MPEG1's fixed frame-rate table; a non-table rate hard-fails open2.
            // maxrate+bufsize MUST be set together (one alone errors) and bufsize (in BITS)
            // must be >= bit_rate/fps; b/7 with maxrate 1.4x caps I-frame transit spikes on
            // the ~1MB/s tunnel. dct=fastint: the forward DCT is scalar C on ARM (no asm).
            let set = |name: &[u8], val: String| {
                let c = std::ffi::CString::new(val).unwrap();
                av_opt_set(ctx as *mut c_void, name.as_ptr() as *const c_char, c.as_ptr(), 0)
            };
            // width/height/pix_fmt BY NAME, not by poking OFF_CTX_*. `video_size` is an
            // IMAGE_SIZE option that writes width and height together, and both it and
            // `pixel_format` live in libavcodec's own option table — confirmed against the
            // device's binary AND present unchanged in 4.x, so unlike a byte offset these three
            // survive an FFmpeg major. The comment that used to justify the pokes claimed no
            // AVOption existed for them; it was simply wrong.
            set(b"video_size\0", format!("{w}x{h}"));
            set(b"pixel_format\0", "yuv420p".into());
            set(b"time_base\0", "1/30".into());
            set(b"b\0", bitrate_bps.to_string());
            set(b"maxrate\0", (bitrate_bps * 7 / 5).to_string());
            set(b"bufsize\0", (bitrate_bps / 7).to_string());
            set(b"g\0", "30".into());
            set(b"bf\0", "0".into());
            set(b"dct\0", "fastint".into());
            let r = avcodec_open2(ctx, codec, std::ptr::null_mut());
            if r < 0 {
                crate::log(&format!("venc: avcodec_open2 failed ({r})"));
                return None; // Drop frees ctx
            }
            // runtime ABI self-check: the offsets above must round-trip through the
            // battle-tested AVCodecParameters model, or we stop before feeding frames.
            let mut par: AVCodecParameters = std::mem::zeroed();
            if avcodec_parameters_from_context(&mut par, ctx) < 0
                || par.width != w || par.height != h || par.format != fmt_yuv
            {
                crate::log(&format!(
                    "venc: ABI self-check FAILED (par {}x{} fmt {} vs {}x{} fmt {}) — mpeg off",
                    par.width, par.height, par.format, w, h, fmt_yuv
                ));
                free_ptr(par.extradata as *mut c_void);
                return None;
            }
            free_ptr(par.extradata as *mut c_void);
            let frame = av_frame_alloc();
            if frame.is_null() {
                return None;
            }
            v.frame = frame;
            poke_i32(frame, OFF_FRAME_WIDTH, w);
            poke_i32(frame, OFF_FRAME_HEIGHT, h);
            poke_i32(frame, OFF_FRAME_FORMAT, fmt_yuv);
            if av_frame_get_buffer(frame, 32) < 0 {
                crate::log("venc: frame buffer alloc failed");
                return None;
            }
            v.sws = sws_getContext(w, h, fmt_rgba, w, h, fmt_nv12, SWS_BILINEAR,
                                   std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null());
            if v.sws.is_null() {
                crate::log("venc: sws_getContext failed");
                return None;
            }
            // ---- muxer over custom AVIO ----
            let mut oc: *mut AVFormatContext = std::ptr::null_mut();
            let r = avformat_alloc_output_context2(&mut oc, std::ptr::null_mut(),
                                                   b"mpegts\0".as_ptr() as *const c_char,
                                                   std::ptr::null());
            if r < 0 || oc.is_null() {
                crate::log(&format!("venc: no mpegts muxer in this build ({r})"));
                return None;
            }
            v.oc = oc;
            let iobuf = av_malloc(32768) as *mut u8;
            if iobuf.is_null() {
                return None;
            }
            let avio = avio_alloc_context(iobuf, 32768, 1,
                                          v.sink.as_mut() as *mut VencSink as *mut c_void,
                                          None, Some(venc_write_cb), None);
            if avio.is_null() {
                free_ptr(iobuf as *mut c_void);
                return None;
            }
            v.avio = avio;
            (*oc).pb = avio;
            let st = avformat_new_stream(oc, std::ptr::null());
            if st.is_null() {
                return None;
            }
            v.st = st;
            let cp = stream_codecpar(st);
            if avcodec_parameters_from_context(cp, ctx) < 0 {
                return None;
            }
            // write_header emits PAT/PMT (re-emitted every 40 TS pkts + each keyframe by
            // default — do NOT set pat_period/sdt_period, that DISABLES the count-based
            // re-emit) and forces st->time_base to 1/90000; read it back for the rescale.
            // sink.fd is still -1 here: header bytes buffer in the 32KB AVIO buffer and
            // reach the socket with the first flushed frame.
            if avformat_write_header(oc, std::ptr::null_mut()) < 0 {
                crate::log("venc: write_header failed");
                return None;
            }
            v.st_tb = stream_time_base(st);
            v.pkt = av_packet_alloc();
            if v.pkt.is_null() {
                return None;
            }
            crate::log(&format!("venc: mpeg1/ts up {}x{} @{}bps (st_tb {}/{})",
                                w, h, bitrate_bps, v.st_tb.num, v.st_tb.den));
            Some(v)
        }
    }

    /// Encode one top-down RGBA frame and push the muxed TS to `fd`.
    /// Returns false when the socket died (caller drops the client) — encoder
    /// errors also return false and the caller reinits on the next frame.
    /// `flip` = the capture buffer is bottom-up (the 2-pass 480x270 chain); sws_scale
    /// takes a NEGATIVE input stride for that, so there is no CPU flip pass.
    pub(crate) fn encode(&mut self, rgba: &[u8], fd: c_int, flip: bool) -> bool {
        unsafe {
            debug_assert!(rgba.len() >= (self.w * self.h * 4) as usize);
            self.sink.fd = fd;
            self.sink.failed = false;
            // the encoder keeps GOP reference frames refcounted on our frame's buffers —
            // make_writable clones them out before we overwrite the pixels (skipping this
            // corrupts inter prediction, it does not crash).
            let t0 = std::time::Instant::now();
            if av_frame_make_writable(self.frame) < 0 {
                return false;
            }
            let row = self.w * 4;
            let (src0, stride0) = if flip {
                (rgba.as_ptr().add(((self.h - 1) * row) as usize), -row)
            } else {
                (rgba.as_ptr(), row)
            };
            let src: [*const u8; 4] = [src0, std::ptr::null(), std::ptr::null(), std::ptr::null()];
            let src_stride: [c_int; 4] = [stride0, 0, 0, 0];
            // NV12 out: Y straight into the frame's Y plane, interleaved UV into scratch
            let fdata = (self.frame as *mut u8).add(OFF_FRAME_DATA) as *mut *mut u8;
            let fls = (self.frame as *mut u8).add(OFF_FRAME_LINESIZE) as *mut c_int;
            let dst: [*mut u8; 4] = [*fdata, self.nv12_uv.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut()];
            let dst_stride: [c_int; 4] = [*fls, self.w, 0, 0];
            sws_scale(self.sws, src.as_ptr(), src_stride.as_ptr(), 0, self.h, dst.as_ptr(), dst_stride.as_ptr());
            // deinterleave UVUV... -> planar U + V (chroma is w/2 x h/2)
            let (cw, chh) = ((self.w / 2) as usize, (self.h / 2) as usize);
            let (u_base, v_base) = (*fdata.add(1), *fdata.add(2));
            let (u_ls, v_ls) = (*fls.add(1) as usize, *fls.add(2) as usize);
            for row in 0..chh {
                let s = &self.nv12_uv[row * self.w as usize..row * self.w as usize + cw * 2];
                let up = u_base.add(row * u_ls);
                let vp = v_base.add(row * v_ls);
                for i in 0..cw {
                    *up.add(i) = s[i * 2];
                    *vp.add(i) = s[i * 2 + 1];
                }
            }
            let t1 = std::time::Instant::now();
            poke_i64(self.frame, OFF_FRAME_PTS, self.pts);
            self.pts += 1;
            let r = avcodec_send_frame(self.ctx, self.frame);
            if r < 0 {
                crate::log(&format!("venc: send_frame failed ({r})"));
                return false;
            }
            loop {
                let r = avcodec_receive_packet(self.ctx, self.pkt);
                if r == AVERROR_EAGAIN || r == AVERROR_EOF {
                    break;
                }
                if r < 0 {
                    crate::log(&format!("venc: receive_packet failed ({r})"));
                    return false;
                }
                av_packet_rescale_ts(self.pkt, VENC_TB, self.st_tb);
                (*self.pkt).stream_index = 0;
                let wr = av_write_frame(self.oc, self.pkt);
                av_packet_unref(self.pkt);
                if wr < 0 || self.sink.failed {
                    return false;
                }
            }
            // per-frame flush: the AVIO buffer otherwise batches ~100ms of TS at 2.5Mbps
            avio_flush(self.avio);
            self.t_sws_us += (t1 - t0).as_micros() as u64;
            self.t_enc_us += t1.elapsed().as_micros() as u64;
            self.t_n += 1;
            if self.t_last_log.elapsed().as_secs_f32() >= 5.0 && self.t_n > 0 {
                crate::log(&format!("venc: {} frm, sws {:.1}ms enc {:.1}ms avg",
                                    self.t_n,
                                    self.t_sws_us as f32 / self.t_n as f32 / 1000.0,
                                    self.t_enc_us as f32 / self.t_n as f32 / 1000.0));
                self.t_sws_us = 0;
                self.t_enc_us = 0;
                self.t_n = 0;
                self.t_last_log = std::time::Instant::now();
            }
            !self.sink.failed
        }
    }
}

impl Drop for Venc {
    fn drop(&mut self) {
        unsafe {
            // no av_write_trailer: the peer is usually already gone, and jsmpeg
            // needs no trailer. Free order: packet, frame, sws, muxer, avio, codec.
            if !self.pkt.is_null() {
                av_packet_free(&mut self.pkt);
            }
            if !self.frame.is_null() {
                av_frame_free(&mut self.frame);
            }
            if !self.sws.is_null() {
                sws_freeContext(self.sws);
            }
            if !self.oc.is_null() {
                avformat_free_context(self.oc); // does not free the caller-set pb
            }
            if !self.avio.is_null() {
                free_avio(self.avio);
            }
            if !self.ctx.is_null() {
                avcodec_free_context(&mut self.ctx);
            }
        }
    }
}

/// How a subtitle stream's payload turns into displayable text — classified by the codec's
/// name (avcodec_get_name is already linked, so we avoid hardcoding the n3.3 subtitle codec-id
/// block, which the ABI probe never verified). Bitmap subs (PGS/VobSub/DVB/teletext) carry no
/// text and can't be client-rendered, but still occupy their file-order slot so the track
/// menu's desired_sub_idx stays aligned with the metadata subs list.
#[derive(Clone, Copy, PartialEq)]
enum SubKind {
    Plain,   // SRT / subrip / text / webvtt: packet payload is UTF-8 text
    Ass,     // ASS / SSA: dialogue line; text is the field after the 8th comma
    MovText, // mp4 tx3g: 2-byte big-endian text-length prefix, then UTF-8 text
    Bitmap,  // PGS / VobSub / DVB / teletext: image subtitle, not renderable here
}

unsafe fn sub_kind(codec_id: c_int) -> SubKind {
    let name = std::ffi::CStr::from_ptr(avcodec_get_name(codec_id)).to_string_lossy();
    match name.as_ref() {
        "ass" | "ssa" => SubKind::Ass,
        "mov_text" => SubKind::MovText,
        "subrip" | "srt" | "text" | "webvtt" | "vplayer" | "pjs" | "jacosub" | "microdvd"
        | "sami" | "realtext" | "subviewer" | "subviewer1" | "stl" | "mpl2" => SubKind::Plain,
        _ => SubKind::Bitmap,
    }
}

/// Open a software decoder for an image-subtitle stream (PGS/VobSub/DVB). Returns a
/// lib-allocated AVCodecContext (free with avcodec_free_context) or null if the build
/// lacks the decoder / open fails. parameters_to_context carries extradata — dvdsub needs
/// the palette from it, so this must run before open2.
unsafe fn open_sub_decoder(cp: *const AVCodecParameters) -> *mut AVCodecContext {
    let codec = avcodec_find_decoder((*cp).codec_id);
    if codec.is_null() {
        crate::player::log(&format!("ff: no image-sub decoder for codec_id={}", (*cp).codec_id));
        return std::ptr::null_mut();
    }
    let ctx = avcodec_alloc_context3(codec);
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    if avcodec_parameters_to_context(ctx, cp) < 0 || avcodec_open2(ctx, codec, std::ptr::null_mut()) < 0 {
        let mut c = ctx;
        avcodec_free_context(&mut c);
        return std::ptr::null_mut();
    }
    ctx
}

/// The subtitle stream's AUTHORING CANVAS (the coordinate space the decoded rects' x/y/w/h are
/// expressed in), or (0,0) if this decoder never declared one. 1920×1080 for Blu-ray PGS,
/// 720×480/576 for a DVD VobSub rip, 3840×2160 for some 4K PGS — assuming 1080p unconditionally
/// is what made VobSub land as a postage stamp in the corner.
///
/// Read WITHOUT a raw struct poke: `avcodec_parameters_from_context` copies the decoder's
/// width/height into the AVCodecParameters this crate already models (and whose width/height at
/// +48/+52 the video path has used on-device since the demuxer landed), so the whole read runs
/// inside the library's own code and needs no new ABI offset.
///
/// ABI proof (device's own `libavcodec.so.57.89.100`, disassembled 2026-07-29 — the build ships
/// stripped, so this is the primary evidence, not a header):
/// `avcodec_parameters_from_context+0x88` is `cmp r3,#3 / beq +0x13c` (AVMEDIA_TYPE_SUBTITLE ==
/// 3) and `+0x13c` is exactly `ldr r2,[r5,#124] / ldr r3,[r5,#128] / str r2,[r4,#48] /
/// str r3,[r4,#52]` — so THIS build does carry the subtitle case, and it corroborates
/// `OFF_CTX_WIDTH`/`OFF_CTX_HEIGHT` (124/128) and AVCodecParameters.width/height (48/52) at the
/// same time. The prologue's `memset(par, 0, 136)` likewise confirms the modeled sizeof = 136.
///
/// Must be called AFTER a decode: PGS carries the canvas in the presentation composition segment,
/// so pgssubdec only sets it while decoding (dvdsub sets it at open, from the .idx `size:` line).
unsafe fn sub_canvas(dec: *mut AVCodecContext) -> (i32, i32) {
    let par = avcodec_parameters_alloc();
    if par.is_null() {
        return (0, 0);
    }
    let wh = if avcodec_parameters_from_context(par, dec) >= 0 {
        ((*par).width, (*par).height)
    } else {
        (0, 0)
    };
    let mut p = par;
    avcodec_parameters_free(&mut p);
    // A canvas we cannot make sense of is worse than none: report unknown and let the renderer
    // fall back to 1:1 rather than scale the cue by a garbage ratio. The window spans every real
    // authoring canvas with room to spare (the smallest in the wild is DVD's 720×480) and rejects
    // a decoder that reports a rect size, a zero, or an uninitialised field.
    const MIN: c_int = 160;
    const MAX: c_int = 8192;
    if wh.0 < MIN || wh.1 < MIN || wh.0 > MAX || wh.1 > MAX {
        (0, 0)
    } else {
        wh
    }
}

/// Convert one decoded PAL8 subtitle rect to a straight-alpha RGBA bitmap (palette entries are
/// 0xAARRGGBB), or None if the decoder left it unusable. Coords are passed through in the
/// stream's own authoring canvas — the renderer scales, not us.
///
/// Every field here is unvalidated data from a decoder fed by the network, and `usize` is 32
/// bits on this target, so the size is bounded BEFORE it is multiplied: `w*h*4` for a rect the
/// decoder claimed was 40000×40000 wraps to a small allocation, and the write loop would then
/// run off the end of it (a panic on the demux thread, which is outside `ui::guard`).
unsafe fn rect_to_rgba(r: *const AVSubtitleRect) -> Option<crate::player::SubRect> {
    if r.is_null() {
        return None;
    }
    let (x, y, w, h, stride) = ((*r).x, (*r).y, (*r).w, (*r).h, (*r).linesize[0]);
    let idx = (*r).data[0];
    let pal = (*r).data[1] as *const u32;
    // no real subtitle bitmap approaches 8192 on a side — the largest authoring canvas in the
    // wild is 4K, and a rect cannot usefully exceed its own canvas
    const MAX_SIDE: c_int = 8192;
    if idx.is_null() || pal.is_null() || w <= 0 || h <= 0 || stride < w || w > MAX_SIDE || h > MAX_SIDE {
        return None;
    }
    let (wu, hu, su) = (w as usize, h as usize, stride as usize);
    let bytes = match wu.checked_mul(hu).and_then(|n| n.checked_mul(4)) {
        Some(n) => n,
        None => return None,
    };
    let mut rgba = vec![0u8; bytes];
    for row in 0..hu {
        let src = idx.add(row * su);
        for col in 0..wu {
            let p = *pal.add(*src.add(col) as usize); // 0xAARRGGBB (native u32)
            let o = (row * wu + col) * 4;
            rgba[o] = (p >> 16) as u8; // R
            rgba[o + 1] = (p >> 8) as u8; // G
            rgba[o + 2] = p as u8; // B
            rgba[o + 3] = (p >> 24) as u8; // A
        }
    }
    Some(crate::player::SubRect { x, y, w, h, rgba })
}

/// Decode one image-subtitle packet for the SELECTED track and push it to the render store.
/// A CLEAR (num_rects==0) closes the open cue; otherwise EVERY rect of the display set is
/// converted to straight-alpha RGBA and pushed as one cue with start = packet pts (the end is
/// set later by the next CLEAR or superseding set). Two-line dialogue and sign-plus-dialogue are
/// authored as separate rects of the SAME display set, so dropping all but rect 0 (what this did
/// before) silently lost half the line. The set's canvas comes from `sub_canvas`.
unsafe fn decode_bitmap_cue(dec: *mut AVCodecContext, pkt: *mut AVPacket, track: c_int, st: *mut AVStream) {
    let mut sub: AVSubtitle = std::mem::zeroed();
    let mut got: c_int = 0;
    if avcodec_decode_subtitle2(dec, &mut sub, &mut got, pkt) < 0 || got == 0 {
        return;
    }
    let pts = pts_ns(pkt, st);
    if sub.num_rects == 0 {
        crate::player::close_subtitle_bitmap(track, pts);
        avsubtitle_free(&mut sub);
        return;
    }
    // A pathological display set cannot be allowed to bloat the 24 MB store or the renderer's
    // texture set; DVB regions are the realistic source of many rects, PGS allows at most 2.
    const MAX_RECTS: usize = 8;
    let n = (sub.num_rects as usize).min(MAX_RECTS);
    let mut rects = Vec::with_capacity(n);
    for i in 0..n {
        if let Some(r) = rect_to_rgba(*sub.rects.add(i)) {
            rects.push(r);
        }
    }
    if !rects.is_empty() {
        let (cw, ch) = sub_canvas(dec);
        if track == crate::player::desired_sub_idx() {
            let r0 = &rects[0];
            crate::player::log(&format!(
                "image cue [{}ms] {}x{} at {},{} rects={} canvas={cw}x{ch}",
                pts / 1_000_000,
                r0.w,
                r0.h,
                r0.x,
                r0.y,
                rects.len()
            ));
        }
        crate::player::push_subtitle_bitmap(track, pts, cw, ch, rects);
        if sub.num_rects as usize > MAX_RECTS {
            crate::player::log(&format!(
                "ff: image-sub track#{track} {} rects (capped at {MAX_RECTS})",
                sub.num_rects
            ));
        }
    }
    avsubtitle_free(&mut sub);
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

/// Resolve the four FFmpeg libraries by SONAME candidate list. `false` means this device has no
/// FFmpeg we can use, and NOTHING in this module may be called — not even `avformat_version`.
///
/// Kept separate from the version check that follows it because the two failures are different
/// facts about the television and deserve different log lines: "there is no libavformat here at
/// all" versus "there is one and it is the wrong major".
fn load_libraries() -> bool {
    let mut ok = true;
    for (what, verdict) in [
        ("avformat", avformat::load()),
        ("avcodec", avcodec::load()),
        ("avutil", avutil::load()),
        ("swscale", swscale::load()),
    ] {
        match verdict {
            crate::dynlib::Loaded::Ok(soname) => crate::log(&format!("ff: bound {what} -> {soname}")),
            crate::dynlib::Loaded::NoLibrary => {
                ok = false;
                crate::log(&format!("ff: no {what} on this device (tried the 4 known majors)"));
            }
            crate::dynlib::Loaded::Incomplete(soname, n) => {
                ok = false;
                crate::log(&format!("ff: {soname} is missing {n} symbol(s) we need — named above"));
            }
        }
    }
    ok
}

/// Boot smoke test + optional ABI probe. Called once at startup.
pub(crate) fn boot() {
    if !load_libraries() {
        ABI_OK.store(false, std::sync::atomic::Ordering::Relaxed);
        crate::log("ff: FFmpeg unavailable — the app runs, playback will refuse");
        return;
    }
    unsafe {
        let (fmt, cod, utl) = (avformat_version(), avcodec_version(), avutil_version());
        crate::player::log(&format!(
            "ff: avformat={} avcodec={} avutil={}",
            ver(fmt),
            ver(cod),
            ver(utl)
        ));
        // THE MAJORS ARE THE ABI. Everything below reads FFmpeg structs at offsets fixed to the
        // n3.3 layout that webOS 4.x ships (libavformat 57 / libavcodec 57 / libavutil 55), and
        // FFmpeg only guarantees layout within a major — minors may APPEND, majors reorder.
        //
        // This used to log the versions and gate nothing, which is worse than not reading them:
        // on a webOS 5 set (libavformat 58) `sizeof(AVStream)` is 688 while OFF_STREAM_CODECPAR
        // is 708, so the demuxer would read 20 bytes PAST the struct and dereference whatever it
        // found as an `AVCodecParameters *` — on a device with no debugger. Refusing is the only
        // safe answer, because a wrong offset does not fail, it succeeds with garbage.
        let bad = (fmt >> 16, cod >> 16, utl >> 16) != (57, 57, 55);
        ABI_OK.store(!bad, std::sync::atomic::Ordering::Relaxed);
        if bad {
            crate::player::log(
                "ff: UNSUPPORTED FFmpeg majors (need avformat 57 / avcodec 57 / avutil 55) \
                 — the struct offsets this demuxer uses are n3.3-only; refusing to demux",
            );
        }
    }
    // Phase A dev trigger: /tmp/plxnative-ffprobe holds a media URL to open + dump streams,
    // confirming the FFmpeg-3.3 struct offsets against known media before we build on them.
    if let Some(u) = crate::dev::read("ffprobe") {
        if !u.is_empty() {
            probe(&u);
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
        // interrupt: bail out of a blocked read on teardown (aborted) only. A seek does NOT
        // interrupt the read — the demux thread services it itself between two av_read_frame
        // calls (see the read loop), so there is nothing to unblock and nothing to race.
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
        // Teardown may have raced us here — the same race the reopen site in `demux` guards, and
        // this is the third way in. Teardown aborts the lanes, fires ONE `http_shutdown` at this
        // socket, then JOINS this thread from the main thread. Our AVIO is SEEKABLE, so
        // libavformat treats the broken read as recoverable and heals it by calling US (the
        // 5938b5f mechanism, spelled out in the read loop's note below). Without this check it
        // gets a BRAND NEW connection the already-fired shutdown cannot touch — and because
        // `read_cb` then reports EOF again, that recovery repeats: measured on the host at ONE
        // full `http_open` per hop, so the main thread's join is held for a whole reopen every
        // time round, not just once. `read_cb` has bailed on this flag since it was written; a
        // seek was the way around it.
        //
        // Placed AFTER the AVSEEK_SIZE branch on purpose: a size query is a field read, not I/O,
        // and answering it during teardown costs nothing.
        if crate::aq::aq_is_aborted(s.aq) {
            return -1;
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

/// The libavformat demuxer thread body (spawned by engine::start_bufferfeed).
/// Opens the URL through a custom AVIO over stream.rs, reads packets, converts video to
/// Annex-B via the mp4toannexb BSF (VPS/SPS/PPS prepended at every keyframe), feeds video
/// (es=1) + raw audio (es=2) to the AuQueue, and seeks via av_seek_frame.
/// Parse an avcC (H264) / hvcC (HEVC) extradata record into a ready-to-prepend Annex-B
/// parameter-set blob (VPS/SPS/PPS with 4-byte start codes) + the NAL length-prefix size.
/// (Ported from the retired mkv.rs demuxer) — the format the Starfish decoder wants.
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

/// End offset of a NAL of length `nl` starting at `i`, or None if it is empty or would run
/// past `size`. **The width is load-bearing**: `usize` is 32 bits on the TV, `nls` is up to 4,
/// so a hostile/corrupt length up to `0xFFFF_FFFF` makes the natural `i + nl > size` guard WRAP
/// to a small number, pass its own bounds check, and panic the demux thread inside the slice —
/// killing the producer before it can EOF the queues, which hangs playback with no error path.
/// Computing in `u64` makes the bound hold on every target. Never inline this back to `i + nl`.
fn nal_end(i: usize, nl: usize, size: usize) -> Option<usize> {
    if nl == 0 {
        return None;
    }
    let end = i as u64 + nl as u64;
    if end > size as u64 {
        return None;
    }
    Some(end as usize)
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
        if nal_end(i, nl, size).is_none() {
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
        let Some(end) = nal_end(i, nl, size) else {
            break;
        };
        let nal = &d[i..end];
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

/// ADTS sampling_frequency_index (4 bits) for a sample rate; None if not a standard AAC rate.
fn adts_freq_index(rate: c_int) -> Option<u8> {
    Some(match rate {
        96000 => 0, 88200 => 1, 64000 => 2, 48000 => 3, 44100 => 4, 32000 => 5,
        24000 => 6, 22050 => 7, 16000 => 8, 12000 => 9, 11025 => 10, 8000 => 11, 7350 => 12,
        _ => return None,
    })
}

/// (freq_idx, chan_cfg) for the ADTS header, read from the stream's AudioSpecificConfig when
/// present. HE-AAC is why this parses the ASC instead of trusting codecpar: the ASC's FIRST
/// samplingFrequencyIndex is the AAC **core** rate (e.g. 24 kHz) while `codecpar.sample_rate`
/// reports the SBR **output** rate (48 kHz). ADTS must carry the core rate (+ LC profile, SBR
/// stays implicit) so the decoder up-samples through the SBR extension — a 48 kHz header makes
/// it decode each frame as 1024 plain-LC samples into a 2048-sample slot, an audible gap/crackle
/// every frame (Maxton Hall, HE-AAC 5.1). Falls back to codecpar for ASC-less streams.
unsafe fn adts_params(acp: *const AVCodecParameters) -> Option<(u8, u8)> {
    let chan_fallback = {
        let ch = (*acp).channels;
        if (1..=7).contains(&ch) { ch as u8 } else { 2 }
    };
    // AudioSpecificConfig: aot(5) freqIdx(4) [+24-bit rate if idx==15] chanCfg(4) …
    let (ed, n) = ((*acp).extradata, (*acp).extradata_size);
    if !ed.is_null() && n >= 2 {
        let (b0, b1) = (*ed as u32, *ed.add(1) as u32);
        let aot = (b0 >> 3) & 0x1F;
        let freq_idx = (((b0 & 0x07) << 1) | (b1 >> 7)) as u8;
        if aot != 31 && freq_idx <= 12 {
            let chan = ((b1 >> 3) & 0x0F) as u8;
            let ch = if (1..=7).contains(&chan) { chan } else { chan_fallback };
            return Some((freq_idx, ch));
        }
        // escape-coded AOT / explicit 24-bit rate — exotic; fall through to codecpar
    }
    adts_freq_index((*acp).sample_rate).map(|fi| (fi, chan_fallback))
}

/// 7-byte ADTS header for a raw AAC frame of `payload_len` bytes. LG's Starfish decodes
/// ADTS-framed AAC (ss4s: aacInfo format="adts"); mp4/matroska carry RAW AAC, so we reframe.
/// Emits AAC-LC (ADTS profile 1 = object type 2 - 1); buffer fullness = VBR (0x7FF).
fn adts_header(freq_idx: u8, chan_cfg: u8, payload_len: usize) -> [u8; 7] {
    let frame_len = (payload_len + 7) as u32; // total incl. header, 13 bits
    [
        0xFF,
        0xF1, // sync(1111) | MPEG-4(0) | layer(00) | protection_absent(1)
        (1 << 6) | (freq_idx << 2) | (chan_cfg >> 2), // profile(01=LC) | freq_idx | chan hi bit
        ((chan_cfg & 3) << 6) | ((frame_len >> 11) as u8 & 0x03),
        ((frame_len >> 3) & 0xFF) as u8,
        (((frame_len & 0x07) as u8) << 5) | 0x1F, // frame_len lo 3 | fullness hi 5 (11111)
        0xFC, // fullness lo 6 (111111) | num_blocks(00)
    ]
}

pub(crate) fn demux(host: String, port: c_int, path: String, acodec: String, aq: SendPtr<AuQueue>, aqa: SendPtr<AuQueue>, hs: SendPtr<HttpStream>) {
    // Refuse rather than read a struct whose shape we do not know (see `boot`). EOF must still be
    // set on both lanes or `pump` waits forever on a queue nothing will ever fill — the same
    // contract the panic barrier below exists to keep.
    if !ABI_OK.load(Ordering::Relaxed) {
        crate::player::log("demux: refusing — FFmpeg ABI on this device is not the one built for");
        crate::aq::aq_set_eof(aq.0);
        crate::aq::aq_set_eof(aqa.0);
        return;
    }
    DIAG_FIRST.store(true, Ordering::Relaxed);
    ensure_registered();
    let aq_p = aq.0; // VIDEO lane (also the AVIO abort ptr + EOF marker)
    let aqa_p = aqa.0; // AUDIO lane (es=2) — always a distinct queue on the ff (two-lane) path
    let hs_p = hs.0;
    let host_c = CString::new(host).unwrap_or_default();
    let path_c = CString::new(path).unwrap_or_default();

    // PANIC BARRIER around the whole producer body. Not about the unwind itself — this thread is
    // started by `task::spawn`, so std already catches a panic at the thread boundary and turns it
    // into the `Err` that `task::join` logs. The problem is what the unwind SKIPS: the two
    // `aq_set_eof` calls in the tail below. The consumer is a one-way FIFO with no other liveness
    // signal — `aq_pop` reports EOF only from the `eof` flag the producer sets — so a producer that
    // dies without setting it leaves `pump` feeding a queue that will never fill and never end:
    // no EOS is ever pushed (`engine.rs`'s EOS is keyed on the video lane's true EOF), no error is
    // ever surfaced, and the app sits on a frozen picture until BACK. That is strictly worse than
    // the crash it came from — a rare panic became a common-looking hang with no log line pointing
    // at it. So: catch it, name it in the event log, and then run the SAME teardown the normal exit
    // runs, so the pump terminates the way it does at end-of-stream.
    //
    // `AssertUnwindSafe` is honest here rather than a rubber stamp: the state the closure mutates
    // across the boundary is the two AU queues (their own pthread mutexes, and this is their only
    // producer, which is now dead) and `SHARED`'s atomics. What a panic DOES leak is the FFmpeg
    // side — `AVFormatContext`/`AVIOContext`/`AVPacket` are freed at the end of the block, not by a
    // `Drop`, so an unwind past them leaks them plus the socket buffer. That is deliberate: the
    // alternative is calling `avformat_close_input` on a context whose invariants a panicking
    // libavformat may have left broken, and a one-off leak on a path that is meant never to run is
    // cheaper than a segfault. The session ends here either way.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    // Run-once breakable block, NOT a retry loop: every `break` below is an early exit to the
    // EOF/cleanup tail after it. It used to be a real loop that reopened the whole
    // AVFormatContext on a fresh URL, which is how a direct-play seek was supposed to reach its
    // target — but the reopen could never be triggered (see the seek note in the read loop), and
    // both remaining callers replace the engine outright instead: a transcode seek and an
    // audio-track switch each build a new start.mkv and `reload_transcode`, which spawns a new
    // demux thread. So the reopen had no live writer and no live caller.
    loop {
        unsafe {
            // Teardown may have raced us here (it aborts the lanes, then shutdown(2)s the
            // socket, then JOINS this thread on the main thread). Without this check we would
            // open a BRAND NEW connection that the already-fired shutdown cannot touch, and the
            // main thread would sit in that join for the full connect+recv budget.
            if crate::aq::aq_is_aborted(aq_p) {
                crate::player::log("ff: aborted before reopen");
                break;
            }
            crate::stream::http_close(hs_p);
            if crate::stream::http_open(hs_p, host_c.as_ptr(), port, path_c.as_ptr(), std::ptr::null(), "GET") != 0 {
                crate::player::log(&format!("ff: http_open FAILED status={}", crate::stream::hs_status(hs_p)));
                SHARED.demux_failed.store(true, Ordering::Release);
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
                audio_stream_matching(fmt, &acodec)
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
            // AAC needs ADTS framing for LG's decoder (mp4/mkv carry raw AAC). Precompute the
            // per-frame ADTS fields (freq index + channel config) for the selected audio stream;
            // None => not AAC (or a non-standard rate) => fed verbatim.
            let aac_adts: Option<(u8, u8)> = if ai >= 0 {
                let acp = stream_codecpar(*streams.add(ai as usize));
                if (*acp).codec_id == AV_CODEC_ID_AAC {
                    adts_params(acp)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((fi, ch)) = aac_adts {
                crate::player::log(&format!("ff: AAC → ADTS reframing on (freq_idx={fi} ch={ch})"));
            }
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

            // Client-rendered subtitles (direct-play only): enumerate subtitle streams in FILE
            // order so their 0-based position maps 1:1 to the track menu's desired_sub_idx (which
            // indexes metadata d.subs, i.e. every streamType==3 stream in document order). The
            // selected track is read LIVE in the loop below, so switching subtitles mid-play takes
            // effect with no reopen (parity with mkv.rs's active_sub_track).
            // Each entry: (ffmpeg stream index, kind, decoder ctx). The decoder is non-null
            // only for Bitmap tracks (PGS/VobSub/DVB), which we software-decode to pixels;
            // text tracks carry a null ctx and take the payload path below.
            let mut sub_streams: Vec<(c_int, SubKind, *mut AVCodecContext)> = Vec::new();
            for i in 0..(*fmt).nb_streams {
                let cp = stream_codecpar(*streams.add(i as usize));
                if (*cp).codec_type == AVMEDIA_TYPE_SUBTITLE {
                    let k = sub_kind((*cp).codec_id);
                    let dec = if k == SubKind::Bitmap { open_sub_decoder(cp) } else { std::ptr::null_mut() };
                    sub_streams.push((i as c_int, k, dec));
                }
            }
            if !sub_streams.is_empty() {
                let desc: Vec<String> = sub_streams
                    .iter()
                    .map(|(si, k, _)| {
                        let kn = match k {
                            SubKind::Ass => "ass",
                            SubKind::MovText => "mov_text",
                            SubKind::Plain => "text",
                            SubKind::Bitmap => "image",
                        };
                        format!("#{si}:{kn}")
                    })
                    .collect();
                crate::player::log(&format!(
                    "ff: sub tracks=[{}] selected={}",
                    desc.join(","),
                    crate::player::desired_sub_idx()
                ));
            }

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

            // INNER read loop
            loop {
                // Direct-play seek (and the armed resume, which is just a seek published before
                // the first read): the pump leaves a target ns in SHARED.seek_to_ns and THIS
                // thread — the only one that ever touches `fmt` — av_seek_frame's between two
                // av_read_frame calls. A transcode leaves seek_to_ns=-1 (start.mkv is already
                // 0-based at &offset) and so skips this.
                //
                // It used to be an INTERRUPT instead: the pump shutdown(2)'d the socket to break
                // the read, and the outer loop reopened the URL and seeked the fresh context.
                // That could not work, and the test suite hid it behind inherited resume offsets
                // until 2026-07-28. Our AVIO is SEEKABLE (`seek_cb` reopens with a byte Range),
                // so libavformat treats a read error as recoverable, calls seek_cb, and gets a
                // brand-new connection at the same offset — av_read_frame never returned an
                // error, the inner loop never broke, and no reopen ever happened. Every
                // direct-play seek ran out the stuck-watchdog on PRE-seek packets
                // (`rebase: dropping stale kf` at the old position) and escalated to a full
                // reload. Seeking here needs no interrupt at all, so nothing can race it.
                let seek_ns = SHARED.seek_to_ns.swap(-1, Ordering::Acquire);
                if seek_ns >= 0 {
                    let ts = av_rescale_q(seek_ns, NS_TB, stream_time_base(vst));
                    let sr = av_seek_frame(fmt, vi, ts, AVSEEK_FLAG_BACKWARD);
                    crate::player::log(&format!("ff: seek {}s rv={sr}", seek_ns / 1_000_000_000));
                }
                let r = av_read_frame(fmt, pkt);
                if r < 0 {
                    // Genuine end of stream, or teardown. NOT a seek — a seek is serviced at the
                    // top of this loop and never surfaces as a read error.
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
                    if let Some((freq_idx, chan_cfg)) = aac_adts {
                        // prepend a 7-byte ADTS header so LG's decoder can frame the raw AAC
                        let plen = (*pkt).size as usize;
                        let mut framed = Vec::with_capacity(7 + plen);
                        framed.extend_from_slice(&adts_header(freq_idx, chan_cfg, plen));
                        framed.extend_from_slice(std::slice::from_raw_parts((*pkt).data, plen));
                        crate::aq::aq_push(aqa_p, framed.as_ptr(), framed.len() as c_int, pts, 1, 2);
                    } else {
                        crate::aq::aq_push(aqa_p, (*pkt).data, (*pkt).size, pts, 1, 2); // AUDIO lane
                    }
                    av_packet_unref(pkt);
                } else if let Some(sub_pos) = sub_streams.iter().position(|(sidx, _, _)| *sidx == si) {
                    // Subtitle packet. Push a cue for EVERY text track (tagged with its file-order
                    // index), NOT just the selected one, so a mid-play track switch is instant —
                    // the render filters by desired_sub_idx (active_subtitle). Pushing only the
                    // selected track leaves the buffered ~10-20s (the demuxer reads well ahead of
                    // the playhead) cue-less after a switch. Subtitles carry no ES → never fed to
                    // the pipeline. end_ns is pkt.duration; text subs without one fall back to +4s.
                    let kind = sub_streams[sub_pos].1;
                    if kind == SubKind::Bitmap {
                        // Decode EVERY image-sub track as it's read (like text cues), NOT just the
                        // selected one — the demuxer runs ~10-20s ahead of the playhead, so if we
                        // only started decoding at selection time the on-screen moment was already
                        // read past and subs wouldn't appear until the playhead caught up (the
                        // 10-20s lag). Decoding all tracks means the current cue is already in the
                        // store on enable/switch. Keyed by sub_pos (== desired_sub_idx domain); the
                        // renderer filters by selection. RAM is bounded by the store's byte budget.
                        //
                        // GATED on subs being ON at all: with subtitles Off (the common case) the
                        // continuous per-display-set RLE decode + the up-to-24MB RGBA store were
                        // pure waste on the demux core during 4K playback. Turning subs on starts
                        // decoding from the current read position — a switch between two IMAGE
                        // tracks stays instant; only the off→on moment can wait for the next cue.
                        if crate::player::desired_sub_idx() >= 0 {
                            let dec = sub_streams[sub_pos].2;
                            if !dec.is_null() {
                                decode_bitmap_cue(dec, pkt, sub_pos as c_int, *streams.add(si as usize));
                            }
                        }
                    } else {
                        let sst = *streams.add(si as usize);
                        let start = pts_ns(pkt, sst);
                        let dur = (*pkt).duration;
                        let end = if dur > 0 {
                            start + av_rescale_q(dur, stream_time_base(sst), NS_TB)
                        } else {
                            start + 4_000_000_000
                        };
                        let sz = (*pkt).size.max(0) as usize;
                        if !(*pkt).data.is_null() && sz > 0 {
                            let raw = std::slice::from_raw_parts((*pkt).data, sz);
                            // mp4 tx3g: drop the 2-byte big-endian text-length prefix.
                            let payload: &[u8] = if kind == SubKind::MovText && sz >= 2 {
                                let tl = ((raw[0] as usize) << 8) | raw[1] as usize;
                                &raw[2..2 + tl.min(sz - 2)]
                            } else {
                                raw
                            };
                            crate::player::push_subtitle_cue(
                                sub_pos as i32,
                                start,
                                end,
                                payload,
                                kind == SubKind::Ass,
                            );
                        }
                    }
                    av_packet_unref(pkt);
                } else {
                    av_packet_unref(pkt);
                }
                if crate::aq::aq_is_aborted(aq_p) {
                    break;
                }
            }

            // cleanup this stream (we own pb, so close_input won't free the AVIO)
            for (_, _, dec) in sub_streams.iter_mut() {
                if !dec.is_null() {
                    avcodec_free_context(dec); // frees + nulls; reopened fresh on the next outer pass
                }
            }
            let mut pkt_m = pkt;
            av_packet_free(&mut pkt_m);
            avformat_close_input(&mut fmt);
            free_avio(avio);
            let _ = &state; // keep the AvioState alive until after free_avio
        }

        if unsafe { crate::aq::aq_is_aborted(aq_p) } {
            break;
        }
        break;
    }
    })); // end panic barrier. The body above is deliberately NOT re-indented into the closure: a
         // ~340-line whitespace diff would bury every real change this file ever gets again.

    // The panic path joins the normal one HERE, so both leave the consumer in the same state.
    if let Err(e) = &outcome {
        // The payload is almost always a `&str`/`String` (a slice index, an `unwrap`), and the
        // message is worth far more than the bare fact of a panic: this thread walks packet buffers
        // whose sizes come off the wire, so knowing WHICH one gave way is most of the triage. The
        // C tracer in `main.c` cannot help here — nothing faults, so there is no PC to symbolize.
        let what = e
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| e.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string payload>".to_string());
        crate::player::log(&format!("ff: demux PANICKED — {what}"));
        // The producer is gone before it could publish a duration or a single frame, which is the
        // one case EOF alone cannot resolve: `engine.rs` pushes EOS only once the pipeline has
        // reached `Stage::Streaming`, so a panic during open/find_stream_info would otherwise leave
        // the pump waiting on a stage it can never reach. This is the same flag the `http_open`
        // failure above raises, and `pump` turns it into `PlaybackState::Error` (gated on
        // `frames == 0`, so a panic MID-playback is left to the EOF/EOS path below instead).
        SHARED.demux_failed.store(true, Ordering::Release);
    }
    crate::aq::aq_set_eof(aq_p);
    crate::aq::aq_set_eof(aqa_p); // EOF on the audio lane too
    crate::player::log("ff: demux ended");
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// Build a length-prefixed (AVCC-style) packet: 4-byte big-endian length + payload, repeated.
    fn avcc(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in nals {
            v.extend_from_slice(&(n.len() as u32).to_be_bytes());
            v.extend_from_slice(n);
        }
        v
    }

    fn to_annexb(buf: &[u8], is_hevc: bool, param: &[u8]) -> (bool, Vec<u8>) {
        let mut out = Vec::new();
        let key = unsafe { packet_to_annexb(buf.as_ptr(), buf.len(), 4, is_hevc, param, &mut out) };
        (key, out)
    }

    // -- nal_end: the 32-bit bounds guard -------------------------------------------------

    #[test]
    fn nal_end_accepts_a_nal_that_fits() {
        assert_eq!(nal_end(4, 10, 64), Some(14));
        assert_eq!(nal_end(4, 60, 64), Some(64), "a NAL ending exactly at `size` is valid");
    }

    #[test]
    fn nal_end_rejects_empty_and_overrun() {
        assert_eq!(nal_end(4, 0, 64), None, "a zero-length NAL terminates the walk");
        assert_eq!(nal_end(4, 61, 64), None, "one byte past the end is rejected");
    }

    /// Documents the defect `nal_end` exists to prevent. `usize` is 32 bits on the TV, so the
    /// guard that shipped — `i + nl > size` — WRAPPED for a length near u32::MAX, passed its own
    /// bounds check, and panicked the demux thread inside the slice. This assertion cannot fail
    /// on a 64-bit host (the wrap is unreachable here), so it is a documentation test, not a
    /// regression gate: the real gate is that `nal_end` is a named function whose doc says not to
    /// inline it back. Both halves are asserted so the delta is unambiguous to a future reader.
    #[test]
    fn nal_end_rejects_what_the_old_32bit_guard_accepted() {
        let (i, nl, size) = (4usize, 0xFFFF_FFFCusize, 64usize);
        let old_guard_overruns = (i as u32).wrapping_add(nl as u32) > size as u32;
        assert!(!old_guard_overruns, "on 32-bit the old guard computed 0 and let this through");
        assert_eq!(nal_end(i, nl, size), None, "the width-explicit guard rejects it on every target");
    }

    // -- packet_to_annexb ------------------------------------------------------------------

    #[test]
    fn h264_idr_is_a_keyframe_and_gets_the_parameter_set_prepended() {
        // nal_unit_type is the low 5 bits of byte 0; type 5 == IDR.
        let buf = avcc(&[&[0x65, 0xAA, 0xBB]]);
        let param = [0u8, 0, 0, 1, 0x67, 0x42];
        let (key, out) = to_annexb(&buf, false, &param);
        assert!(key, "0x65 & 0x1f == 5 is an IDR");
        assert!(out.starts_with(&param), "a keyframe AU must carry the SPS/PPS");
        assert_eq!(&out[param.len()..], &[0, 0, 0, 1, 0x65, 0xAA, 0xBB]);
    }

    #[test]
    fn h264_non_idr_is_not_a_keyframe_and_gets_no_parameter_set() {
        let buf = avcc(&[&[0x41, 0x01]]); // type 1, non-IDR slice
        let (key, out) = to_annexb(&buf, false, &[0xDE, 0xAD]);
        assert!(!key);
        assert_eq!(out, vec![0, 0, 0, 1, 0x41, 0x01], "no parameter set on a non-keyframe");
    }

    #[test]
    fn hevc_irap_range_is_detected_as_a_keyframe() {
        // HEVC nal type is bits 1..6 of byte 0; IRAP is 16..=23.
        for t in [16u8, 19, 23] {
            let buf = avcc(&[&[t << 1, 0x01, 0x02]]);
            assert!(to_annexb(&buf, true, &[]).0, "HEVC type {t} is IRAP");
        }
        for t in [1u8, 15, 24] {
            let buf = avcc(&[&[t << 1, 0x01, 0x02]]);
            assert!(!to_annexb(&buf, true, &[]).0, "HEVC type {t} is not IRAP");
        }
    }

    #[test]
    fn every_nal_is_emitted_with_a_start_code() {
        let buf = avcc(&[&[0x41, 0x01], &[0x41, 0x02], &[0x41, 0x03]]);
        let (_, out) = to_annexb(&buf, false, &[]);
        assert_eq!(
            out,
            vec![0, 0, 0, 1, 0x41, 0x01, 0, 0, 0, 1, 0x41, 0x02, 0, 0, 0, 1, 0x41, 0x03]
        );
    }

    /// A length field that claims more bytes than the packet holds must truncate cleanly rather
    /// than panic — this is the ordinary shape of a corrupt or mid-transfer-truncated AU.
    #[test]
    fn a_length_past_the_end_truncates_instead_of_panicking() {
        let mut buf = avcc(&[&[0x41, 0x01]]);
        buf.extend_from_slice(&0xFFFF_FF00u32.to_be_bytes()); // absurd length, no payload
        buf.extend_from_slice(&[0x41, 0x02]);
        let (key, out) = to_annexb(&buf, false, &[]);
        assert!(!key);
        assert_eq!(out, vec![0, 0, 0, 1, 0x41, 0x01], "the good NAL survives, the bad one stops the walk");
    }

    #[test]
    fn a_runt_packet_is_rejected_before_any_indexing() {
        let (key, out) = to_annexb(&[0x00, 0x00], false, &[0xFF]);
        assert!(!key);
        assert!(out.is_empty(), "size < nls + 1 must bail before the walk");
    }

    // -- the AVIO callbacks under teardown -------------------------------------------------
    //
    // These drive `read_cb`/`seek_cb` directly, which works on the host precisely because neither
    // one touches FFmpeg: they are plain `extern "C"` fns over an `AvioState`, whose every field
    // (an HttpStream, an AuQueue, two CStrings, three integers) is ordinary Rust. So long as a
    // test stays off `av_*`, the callbacks link and run here exactly as they do on the TV.

    /// A loopback PMS stand-in that COUNTS accepted connections — the observable that matters,
    /// since "did the callback open a new socket" is the whole question. Each connection gets a
    /// 200 whose 8-byte body arrives inside the header read, so `http_read` serves it from
    /// `HttpStream`'s buffer and never needs the socket again.
    ///
    /// The count is bumped BEFORE the reply is written, so it is already final by the time any
    /// `http_open` against this listener can return — every assertion below is causally ordered
    /// behind that, and needs no sleep and no timing margin.
    fn with_counting_listener(body: impl FnOnce(u16, &std::sync::atomic::AtomicUsize)) {
        use std::io::Write;
        use std::sync::atomic::AtomicUsize;
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        srv.set_nonblocking(true).expect("set_nonblocking"); // so the acceptor can be stopped
        let accepts = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                let mut held = Vec::new(); // hold the peers open: an RST would read as a failed reopen
                while !stop.load(Ordering::Acquire) {
                    match srv.accept() {
                        Ok((mut s, _)) => {
                            accepts.fetch_add(1, Ordering::AcqRel);
                            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH");
                            held.push(s);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(1))
                        }
                        Err(_) => break,
                    }
                }
            });
            // Stop the acceptor on the way out however we leave. A FAILING assertion in `body`
            // unwinds through here and `scope` joins before it reports, so a flag set only on the
            // success path would turn every real failure into a hang instead of a message.
            struct StopAcceptor<'a>(&'a AtomicBool);
            impl Drop for StopAcceptor<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            let _stop_on_exit = StopAcceptor(&stop);
            body(port, &accepts);
        });
    }

    /// An open stream plus the aborted video lane behind its AVIO — the state teardown leaves:
    /// `engine::teardown` aborts both lanes, `http_shutdown`s this socket, then joins the demuxer.
    fn opened_stream_with_aborted_lane(port: u16) -> (Box<HttpStream>, Box<AuQueue>, CString, CString) {
        let ip = CString::new("127.0.0.1").unwrap();
        let path = CString::new("/library/parts/1/file.mkv").unwrap();
        let mut hs = crate::stream::http_stream_boxed();
        let rv = crate::stream::http_open(
            &mut *hs, ip.as_ptr(), port as c_int, path.as_ptr(), std::ptr::null(), "GET",
        );
        assert_eq!(rv, 0, "fixture: the first open must succeed");
        let mut aq = crate::aq::aq_new(1 << 20);
        crate::aq::aq_abort(&mut *aq);
        (hs, aq, ip, path)
    }

    /// Teardown fires ONE `shutdown(2)` at the demux socket, but our AVIO is SEEKABLE, so
    /// libavformat heals the resulting broken read by calling `seek_cb` — which reopened the URL
    /// with a byte Range. That fresh socket is one the already-fired shutdown cannot reach, so the
    /// demuxer reads on while the main thread sits in `stream_th.join()`. `read_cb` has bailed on
    /// an aborted lane since it was written, and the reopen site in `demux` carries the same guard
    /// with a comment naming this exact failure; `seek_cb` was the third way in and had neither.
    ///
    /// The invariant is carried by the ACCEPT COUNT, not the return value — a `seek_cb` that
    /// reopened and then failed for an unrelated reason would also return -1. The trailing
    /// AVSEEK_SIZE assertion pins the guard's PLACEMENT: bolted to the top of `seek_cb` it would
    /// start reporting the stream as unsized, a second behaviour change smuggled in under a
    /// teardown fix.
    #[test]
    fn a_seek_after_teardown_fails_instead_of_opening_a_second_connection() {
        with_counting_listener(|port, accepts| {
            let (mut hs, mut aq, ip, path) = opened_stream_with_aborted_lane(port);
            assert_eq!(accepts.load(Ordering::Acquire), 1, "fixture: exactly one connection so far");
            let mut st = AvioState {
                hs: &mut *hs, aq: &mut *aq, host: ip, port: port as c_int, path, off: 0, size: 8,
            };
            let op = &mut st as *mut AvioState as *mut c_void;

            let rv = seek_cb(op, 4, SEEK_SET);

            assert_eq!(
                accepts.load(Ordering::Acquire), 1,
                "seek_cb opened a SECOND connection during teardown — the one the main thread's \
                 join is waiting on, and the one its shutdown(2) can no longer reach"
            );
            assert_eq!(rv, -1, "an aborted seek must report failure so libavformat stops healing");
            assert_eq!(seek_cb(op, 0, AVSEEK_SIZE), 8,
                       "a size query is not I/O — the guard belongs AFTER that branch");
            crate::stream::http_close(&mut *hs);
            crate::aq::aq_destroy(&mut *aq);
        });
    }

    /// The pair invariant, and the answer to "can an aborted demuxer ping-pong forever?".
    /// `read_cb` returns AVERROR_EOF unconditionally once the lane is aborted, and libavformat's
    /// recovery for a failed read on a seekable AVIO is to seek and read again — so if `seek_cb`
    /// succeeds, every hop of that loop is a full `http_open` (connect + request + header read)
    /// against a PMS the main thread is already waiting on. Measured on the unguarded code: nine
    /// accepts for eight hops, i.e. one whole reopen per hop, not the single wasted open the
    /// static reading of this suggested.
    #[test]
    fn an_aborted_read_and_seek_cannot_ping_pong_into_new_connections() {
        with_counting_listener(|port, accepts| {
            let (mut hs, mut aq, ip, path) = opened_stream_with_aborted_lane(port);
            let mut st = AvioState {
                hs: &mut *hs, aq: &mut *aq, host: ip, port: port as c_int, path, off: 0, size: 8,
            };
            let op = &mut st as *mut AvioState as *mut c_void;
            let mut dst = [0u8; 8];
            let mut reads = Vec::new();
            let mut seeks = Vec::new();
            for _ in 0..8 {
                reads.push(read_cb(op, dst.as_mut_ptr(), dst.len() as c_int));
                seeks.push(seek_cb(op, 4, SEEK_SET)); // 4, not 0: a seek to 0 returns 0, and 0 is success
            }
            assert_eq!(
                accepts.load(Ordering::Acquire), 1,
                "the read/seek recovery loop reconnected once per hop — that is the wedge, \
                 not merely a slow teardown"
            );
            assert!(reads.iter().all(|r| *r == AVERROR_EOF), "aborted reads must all report EOF: {reads:?}");
            assert!(seeks.iter().all(|r| *r == -1), "every hop must refuse the seek: {seeks:?}");
            crate::stream::http_close(&mut *hs);
            crate::aq::aq_destroy(&mut *aq);
        });
    }
}
