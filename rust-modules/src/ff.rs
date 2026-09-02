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
/// Did this demux session hand a single video AU to a lane? Cleared by `demux` on entry.
///
/// The tail uses it to tell "ended cleanly" from "ended having produced nothing", which EOF alone
/// cannot express — and which is the difference between a real end-of-file and the player sitting
/// on "Buffering…" forever with no error.
static PUSHED_ANY: AtomicBool = AtomicBool::new(false);
/// Did libswscale load? It is the one FFmpeg library that is NOT required: `RELEASE=1` leaves it
/// out of the package because only the dev capture stream's RGBA->YUV conversion uses it. `venc`
/// checks this rather than relying on being feature-gated out, so the dependency is enforced
/// where it exists instead of somewhere else.
static SWS_OK: AtomicBool = AtomicBool::new(false);
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
pub enum AVBufferRef {}
// AVStream is opaque here: it is 712 bytes with a large "internal but ABI" block, so
// rather than transcribe every field we read only the three we need at their verified
// n3.3 offsets (index +0, time_base +40, codecpar +708). Phase A confirms the offsets.
pub enum AVStream {}
// ---- The FFmpeg 9.0 ABI, on 32-bit ARM. ----
//
// These were a RUNTIME TABLE selected per libavformat major, because the app read the
// television's own FFmpeg and every webOS release ships a different one (55 / 57 / 58 / 59 / 60
// across webOS 2 to 11). They are plain constants again because the app now ships its own
// FFmpeg: exactly one version exists, under a SONAME that cannot collide with the TV's, and
// `ci/ffabi-assert.c` holds every number here against the very headers the shipped libraries
// were built from.
//
// That is the point of bundling. An offset that is merely *probably* right — derived from a
// version string plus an upstream tarball — becomes one the compiler checked.
//
// `tools/ffabi-dump.sh` re-derives all of them if the bundled version ever changes.
//
// **TWO TABLES, and the axis is POINTER WIDTH rather than FFmpeg version.** The 32-bit arm is the
// television; the 64-bit arm is the desktop simulator (`make sim`), which runs this same code
// against a host build of the SAME FFmpeg 9.0 from the SAME `ci/build-ffmpeg.sh` component list.
// Every difference below is a pointer that got wider or an int64 that moved to keep its
// alignment; nothing here is a version difference, which is exactly why the old runtime
// major-selected table does not come back. `ci/ffabi-assert.c` holds both, `#if`-ed the same way,
// against each build's own headers — so a wrong number is a compile error on the platform it is
// wrong for, and `HOST=1 tools/ffabi-dump.sh` prints the 64-bit half.
#[cfg(target_pointer_width = "32")]
const OFF_STREAM_INDEX: usize = 4; // NB not 0 — FFmpeg 5.0 put `const AVClass *av_class` first
#[cfg(target_pointer_width = "32")]
const OFF_STREAM_CODECPAR: usize = 12;
#[cfg(target_pointer_width = "32")]
const OFF_STREAM_TIME_BASE: usize = 20;
#[cfg(target_pointer_width = "64")]
const OFF_STREAM_INDEX: usize = 8;
#[cfg(target_pointer_width = "64")]
const OFF_STREAM_CODECPAR: usize = 16;
#[cfg(target_pointer_width = "64")]
const OFF_STREAM_TIME_BASE: usize = 32;
/// `AVStream.metadata` — the container's own per-track tags, which is where a track's NAME lives.
///
/// It is read for one reason and it is a data reason, not a rendering one: **PMS does not send the
/// per-track title for an MP4.** Verified live 2026-08-22 against one server holding both — for a
/// Matroska part PMS sends `Stream.title` (`"HDRezka Studio"`, `"Forced"`, `"SDH"`) and the track
/// menu has always drawn it; for an MP4 part it sends no `title` key at all, though the file
/// carries a `name` tag on every track (`"Полные Jaskier"`, `"Форс. iTunes"`, `"Full SDH"`).
/// Matroska spells the tag `title` and MP4 spells it `name`, and Plex's parser maps only the first.
/// So a nine-track MP4 arrives as six rows reading `Русский` and nothing else — the user cannot
/// tell a forced signs track from a full translation, and no amount of care in the UI can invent
/// the difference. `/library/streams/{id}` is 501 and `checkFiles=1` adds nothing; the file is the
/// only source, and we are already holding it open.
#[cfg(target_pointer_width = "32")]
const OFF_STREAM_METADATA: usize = 72;
#[cfg(target_pointer_width = "64")]
const OFF_STREAM_METADATA: usize = 80;
/// `AVFormatContext.duration`, in AV_TIME_BASE units. By offset — see the struct's closing note.
#[cfg(target_pointer_width = "32")]
const OFF_FMT_DURATION: usize = 64;
#[cfg(target_pointer_width = "64")]
const OFF_FMT_DURATION: usize = 104;

/// `AVDictionaryEntry` — two `char *`. Modelled rather than opaque because the whole point of the
/// call is to read both halves; `ci/ffabi-assert.c` holds the layout.
#[repr(C)]
pub struct AVDictionaryEntry {
    key: *const c_char,
    value: *const c_char,
}

/// The only field this app reads past `AVFormatContext.streams`.
#[inline]
unsafe fn fmt_duration(fmt: *const AVFormatContext) -> i64 {
    *((fmt as *const u8).add(OFF_FMT_DURATION) as *const i64)
}

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
    pub buf: *mut AVBufferRef,            // +0
    pub pts: i64,                         // +8
    pub dts: i64,                         // +16
    pub data: *mut u8,                    // +24
    pub size: c_int,                      // +28
    pub stream_index: c_int,              // +32
    pub flags: c_int,                     // +36
    pub side_data: *mut AVPacketSideData, // +40
    pub side_data_elems: c_int,           // +44
    pub duration: i64,                    // +48
    pub pos: i64,                         // +56
    // The tail changed at FFmpeg 5.0: `convergence_duration` went with
    // FF_API_CONVERGENCE_DURATION, and these three arrived. sizeof 72 -> 80. The app never
    // allocates an AVPacket (av_packet_alloc does), so only the offsets above matter — but the
    // size is asserted anyway, because a short model here would be a silent overread.
    pub opaque: *mut c_void,          // +64
    pub opaque_ref: *mut AVBufferRef, // +68
    pub time_base: AVRational,        // +72
}

/// `AVPacketSideData` — one entry of `AVCodecParameters::coded_side_data`. It was an opaque
/// `enum` here for as long as nothing read it; the Dolby Vision configuration record is the first
/// thing that does.
///
/// sizeof = 12 on 32-bit ARM, proven with `ci/ffabi-assert.c`. `size` is a `size_t`, so `usize`
/// is the field's type on both the target and the host rather than a number that happens to match
/// — the one place in this table where the Rust type carries the ABI instead of a comment.
#[repr(C)]
pub struct AVPacketSideData {
    pub data: *mut u8, // +0
    pub size: usize,   // +4
    pub type_: c_int,  // +8  (enum AVPacketSideDataType)
}

/// `AVDOVIDecoderConfigurationRecord` (libavutil/dovi_meta.h), the payload of an
/// `AV_PKT_DATA_DOVI_CONF` side-data entry — what the mp4 `dvcC`/`dvvC` box and the Matroska
/// `DolbyVisionConfiguration` block carry, handed over by BOTH bundled demuxers.
///
/// **Nine plain `uint8_t`, so this is the rare zero-risk read in this file**: no integers wider
/// than a byte, no pointers, nothing to align and nothing to pad, on any target. Field order and
/// count verified against the vendored FFmpeg 9.0 header, and each offset proven for 32-bit ARM
/// in `ci/ffabi-assert.c` — which is cheap insurance rather than doubt, since the assertions cost
/// nothing and the struct is public ABI that could gain a field at any major.
///
/// The upstream spelling is `AVDOVI…`, not `AVDovi…`, and the difference matters only to anyone
/// grepping the header for it.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct AVDOVIDecoderConfigurationRecord {
    pub dv_version_major: u8,              // +0
    pub dv_version_minor: u8,              // +1
    pub dv_profile: u8,                    // +2
    pub dv_level: u8,                      // +3
    pub rpu_present_flag: u8,              // +4
    pub el_present_flag: u8,               // +5
    pub bl_present_flag: u8,               // +6
    pub dv_bl_signal_compatibility_id: u8, // +7
    pub dv_md_compression: u8,             // +8
}

// sizeof = 136
#[repr(C)]
pub struct AVCodecParameters {
    pub codec_type: c_int,                      // +0
    pub codec_id: c_int,                        // +4
    pub codec_tag: u32,                         // +8
    pub extradata: *mut u8,                     // +12
    pub extradata_size: c_int,                  // +16
    pub coded_side_data: *mut AVPacketSideData, // +20
    pub nb_coded_side_data: c_int,              // +24
    pub format: c_int,                          // +28
    pub bit_rate: i64,                          // +32
    pub bits_per_coded_sample: c_int,           // +40
    pub bits_per_raw_sample: c_int,             // +44
    pub profile: c_int,                         // +48
    pub level: c_int,                           // +52
    pub width: c_int,                           // +56
    pub height: c_int,                          // +60
    pub sample_aspect_ratio: AVRational,        // +64
    pub framerate: AVRational,                  // +72
    pub field_order: c_int,                     // +80
    pub color_range: c_int,                     // +84
    pub color_primaries: c_int,                 // +88
    pub color_trc: c_int,                       // +92
    pub color_space: c_int,                     // +96
    pub chroma_location: c_int,                 // +100
    pub video_delay: c_int,                     // +104
    // FFmpeg 7 DELETED the deprecated `channel_layout`/`channels` pair that lived here and left
    // only this struct. `nb_channels` is the replacement for the `channels` int, and reading the
    // old field is not a compile error on a hand-written model — it is a silent read of whatever
    // now occupies +104. Which is the argument for shipping a known FFmpeg in one sentence.
    pub ch_layout: AVChannelLayout, // +112
    pub sample_rate: c_int,         // +136
    pub block_align: c_int,         // +140
    pub frame_size: c_int,          // +144
    pub initial_padding: c_int,     // +148
    pub trailing_padding: c_int,    // +152
    pub seek_preroll: c_int,        // +156
    pub alpha_mode: c_int,          // +160
}

/// `AVChannelLayout`, modelled only as far as `nb_channels` — the one field this app reads.
/// The union that follows is 8-aligned, so a 4-byte pad sits after `nb_channels` on ARM.
#[repr(C)]
pub struct AVChannelLayout {
    pub order: c_int,        // +0
    pub nb_channels: c_int,  // +4
    pub u_mask: u64,         // +8  (union { mask, map })
    pub opaque: *mut c_void, // +16
}

// AVFormatContext truncated after `filename` — we only ever hold a library-returned pointer and
// read leading fields. NEVER stack-allocate this.
//
// It STOPS at `filename` because that is where the two majors stop agreeing: FFmpeg 4.0 inserted
// `char *url` at +1056, pushing start_time to +1064 and duration to +1072. Everything above is
// identical on 3.3 and 4.x. `duration` — the only field past that point this app reads — is
// reached through [`fmt_duration`] at the offset the runtime ABI table gives, rather than by
// declaring it here at an offset that is right on exactly one of the two.
#[repr(C)]
pub struct AVFormatContext {
    pub av_class: *const AVClass,    // +0
    pub iformat: *mut AVInputFormat, // +4
    pub oformat: *mut c_void,        // +8
    pub priv_data: *mut c_void,      // +12
    pub pb: *mut AVIOContext,        // +16
    pub ctx_flags: c_int,            // +20
    pub nb_streams: c_uint,          // +24
    pub streams: *mut *mut AVStream, // +28
                                     // STOPS HERE. Everything past `streams` shifts between majors — `duration` is at +48 on
                                     // FFmpeg 6, +64 on 9, and +1064 on the n3.3 the televisions ship, because 5.0 deleted
                                     // `filename[1024]` and later releases kept rearranging what replaced it. It is the only field
                                     // beyond this point the app reads, so it is read at OFF_FMT_DURATION rather than declared at
                                     // a position that would be quietly wrong after a version bump.
}

// AVBSFContext is NOT modelled. The app does its own AVCC->Annex-B conversion in Rust, so the
// bitstream-filter API is never driven — only `av_bsf_get_by_name` survives, and only to log
// whether the filters exist. Keeping a #[repr(C)] mirror of a struct nothing reads meant
// re-deriving its offsets at every FFmpeg bump to prove a fact that did not matter.

// AVSubtitle (avcodec.h) — one decoded subtitle "display set". sizeof 32 on 32-bit ARM
// (u16 format @0; u32 start/end_display_time @4/@8, ms relative to `pts`; num_rects @12;
// rects ptr @16; i64 pts @24 in AV_TIME_BASE µs). repr(C) inserts the +20 pad before pts.
#[repr(C)]
pub struct AVSubtitle {
    pub format: u16,                     // +0  (0 = graphics/bitmap)
    pub start_display_time: u32,         // +4
    pub end_display_time: u32,           // +8
    pub num_rects: c_uint,               // +12
    pub rects: *mut *mut AVSubtitleRect, // +16
    pub pts: i64,                        // +24
}

// AVSubtitleRect (avcodec.h, libavcodec 57 / FFmpeg 3.x). CRUCIAL ABI detail: with
// FF_API_AVPICTURE (major < 58, i.e. this build) a DEPRECATED `AVPicture pict` is embedded
// after nb_colors — 8 data ptrs + 8 linesizes (64 bytes) — which shifts the live `data[4]`
// /`linesize[4]` down by 64. The Pass-A probe logs both the `data[]` and the `pict_data[]`
// slots to confirm which the decoder populates before Pass B reads pixels. sizeof = 132.
#[repr(C)]
pub struct AVSubtitleRect {
    pub x: c_int,         // +0
    pub y: c_int,         // +4
    pub w: c_int,         // +8
    pub h: c_int,         // +12
    pub nb_colors: c_int, // +16
    // +20 — was +84 on the TV's FFmpeg 3.3, because an entire deprecated AVPicture sat ahead of
    // it under FF_API_AVPICTURE. FFmpeg 5.0 deleted that, taking sizeof from 132 to 68.
    pub data: [*mut u8; 4], // +20  data[0]=PAL8 indices, data[1]=palette (256×BGRA)
    pub linesize: [c_int; 4], // +36
    // **`flags` comes BEFORE `type`, and this model had them the other way round until
    // 2026-08-28.** The consequence was latent rather than live — `rect_to_rgba` reads only
    // x/y/w/h/linesize[0]/data[0..2], all of which are ahead of the swap — but it was wrong in
    // the way this whole apparatus exists to prevent: `type_` read `flags`, `text` read `type`,
    // and on the 64-bit host `flags` landed at offset 96 of a 96-byte struct, i.e. one word past
    // the end. Found by porting the table to the simulator's pointer width, which is a second
    // independent reading of the same header and is exactly what caught it. `ci/ffabi-assert.c`
    // now pins all four on both widths.
    pub flags: c_int,      // +52 arm / +72 host
    pub type_: c_int,      // +56 / +76 (enum AVSubtitleType: 0=NONE, 1=BITMAP, 2=TEXT, 3=ASS)
    pub text: *mut c_char, // +60 / +80
    pub ass: *mut c_char,  // +64 / +88
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
// One consequence worth knowing: the `cfg(test)` gate is gone. With no link directive the host
// suite links unconditionally, and a test that calls into FFmpeg now fails by taking `dlopen`'s
// None branch on Darwin instead of by failing to link.
crate::dynlib! {
    avformat: [
        // The ELF the app SHIPS, and the Mach-O the desktop simulator builds beside it
        // (`HOST=1 ci/build-ffmpeg.sh` — same version, same component list). A candidate
        // LIST rather than a `cfg`: `dynlib!` already tries each name and reports the ones
        // it could not open, and only one of these two can ever exist in an app directory.
        "libavformat-plx.so.63",
        "libavformat-plx.63.dylib",
    ] {
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
    avcodec: [
        // The ELF the app SHIPS, and the Mach-O the desktop simulator builds beside it
        // (`HOST=1 ci/build-ffmpeg.sh` — same version, same component list). A candidate
        // LIST rather than a `cfg`: `dynlib!` already tries each name and reports the ones
        // it could not open, and only one of these two can ever exist in an app directory.
        "libavcodec-plx.so.63",
        "libavcodec-plx.63.dylib",
    ] {
    fn avcodec_version() -> c_uint;
    fn av_packet_alloc() -> *mut AVPacket;
    fn av_packet_free(pkt: *mut *mut AVPacket);
    fn av_packet_unref(pkt: *mut AVPacket);
    fn avcodec_get_name(id: c_int) -> *const c_char;
    fn av_bsf_get_by_name(name: *const c_char) -> *const AVBitStreamFilter;
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
    avutil: [
        // The ELF the app SHIPS, and the Mach-O the desktop simulator builds beside it
        // (`HOST=1 ci/build-ffmpeg.sh` — same version, same component list). A candidate
        // LIST rather than a `cfg`: `dynlib!` already tries each name and reports the ones
        // it could not open, and only one of these two can ever exist in an app directory.
        "libavutil-plx.so.61",
        "libavutil-plx.61.dylib",
    ] {
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
    // Container tags. Borrowed, never freed: the entry belongs to the dictionary, which belongs to
    // the AVStream, which `avformat_close_input` frees — so every read must copy before the close.
    fn av_dict_get(
        m: *const AVDictionary,
        key: *const c_char,
        prev: *const AVDictionaryEntry,
        flags: c_int,
    ) -> *const AVDictionaryEntry;
}}
crate::dynlib! {
    swscale: [
        // The ELF the app SHIPS, and the Mach-O the desktop simulator builds beside it
        // (`HOST=1 ci/build-ffmpeg.sh` — same version, same component list). A candidate
        // LIST rather than a `cfg`: `dynlib!` already tries each name and reports the ones
        // it could not open, and only one of these two can ever exist in an app directory.
        "libswscale-plx.so.10",
        "libswscale-plx.10.dylib",
    ] {
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
// The only codec id the app compares against. The others (H264, AC3, E-AC3) and AV_PKT_FLAG_KEY
// were declared and never read; their values shift between FFmpeg majors, so each one was a
// constant to re-derive and assert in order to prove nothing.
pub const AV_CODEC_ID_AAC: c_int = 0x15002;
pub const AV_CODEC_ID_H264: c_int = 27;
// FF_API_XVMC and FF_API_VOXWARE both died before FFmpeg 6, which is why these differ from the
// values the n3.3 televisions use (28 / 174 / 0x15029).
pub const AV_CODEC_ID_HEVC: c_int = 172;
pub const AV_PKT_FLAG_KEY: c_int = 1;
pub const AVSEEK_FLAG_BACKWARD: c_int = 1;
pub const AVERROR_EOF: c_int = -541478725;
pub const AVERROR_EAGAIN: c_int = -11;
/// `AVERROR(EIO)`: unlike EOF, tells libavformat and our producer that the source was truncated by
/// a transport failure.
pub const AVERROR_IO: c_int = -5;
pub const AV_NOPTS_VALUE: i64 = i64::MIN;
pub const AV_TIME_BASE: i64 = 1_000_000;
pub const NS_TB: AVRational = AVRational {
    num: 1,
    den: 1_000_000_000,
};
/// `AV_PKT_DATA_DOVI_CONF` — the side-data type tag whose payload is an
/// [`AVDOVIDecoderConfigurationRecord`]. 29 in FFmpeg 9.0; asserted in `ci/ffabi-assert.c`
/// because `AVPacketSideDataType` is an ordinary sequential enum and every value in it shifts
/// when a member is inserted above.
pub const AV_PKT_DATA_DOVI_CONF: c_int = 29;

// ---- AVStream field accessors, at the constants above. Read by offset rather than modelled:
// the struct is large, mostly internal, and only three fields are wanted. ----
#[inline]
unsafe fn stream_codecpar(s: *mut AVStream) -> *mut AVCodecParameters {
    *((s as *const u8).add(OFF_STREAM_CODECPAR) as *const *mut AVCodecParameters)
}
#[inline]
unsafe fn stream_time_base(s: *mut AVStream) -> AVRational {
    *((s as *const u8).add(OFF_STREAM_TIME_BASE) as *const AVRational)
}

/// PURE: read an [`AVDOVIDecoderConfigurationRecord`] out of the raw side-data bytes.
///
/// Split from the pointer walk in [`dovi_conf`] on purpose — this half is the only half worth
/// testing, and it is testable precisely because it never touches FFmpeg. (A test that had to
/// enter the library would take `dlopen`'s `None` branch on Darwin and pass without executing
/// anything, which is the failure shape `docs/agent-reference.md` warns about by name.)
///
/// A SHORT buffer yields `None` rather than a partial record. FFmpeg allocates these with
/// `av_dovi_alloc` and its own demuxers always write all nine bytes, so this cannot happen today
/// — but the record is public ABI whose size "is not a part of the public ABI" by its own header's
/// admission, and reading nine bytes out of a seven-byte allocation is a heap overread that would
/// report a plausible profile number rather than crash. A longer buffer is fine and expected: a
/// future FFmpeg may append fields, and the nine we read keep their meaning.
fn parse_dovi_conf(bytes: &[u8]) -> Option<AVDOVIDecoderConfigurationRecord> {
    if bytes.len() < 9 {
        return None;
    }
    Some(AVDOVIDecoderConfigurationRecord {
        dv_version_major: bytes[0],
        dv_version_minor: bytes[1],
        dv_profile: bytes[2],
        dv_level: bytes[3],
        rpu_present_flag: bytes[4],
        el_present_flag: bytes[5],
        bl_present_flag: bytes[6],
        dv_bl_signal_compatibility_id: bytes[7],
        dv_md_compression: bytes[8],
    })
}

/// The Dolby Vision configuration record attached to this stream, if the demuxer found one.
///
/// `coded_side_data` is the STREAM-level side data (as opposed to `AVPacket::side_data`, which is
/// per-packet), and both bundled demuxers populate it: `mov` from the `dvcC`/`dvvC` sample-entry
/// box, `matroska` from the `DolbyVisionConfiguration` block-additions element. So this answers
/// "what is this file really" for every container the app direct-plays, without decoding a frame.
unsafe fn dovi_conf(cp: *const AVCodecParameters) -> Option<AVDOVIDecoderConfigurationRecord> {
    let (list, n) = ((*cp).coded_side_data, (*cp).nb_coded_side_data);
    if list.is_null() || n <= 0 {
        return None;
    }
    for i in 0..n as usize {
        let sd = &*list.add(i);
        if sd.type_ != AV_PKT_DATA_DOVI_CONF || sd.data.is_null() {
            continue;
        }
        return parse_dovi_conf(std::slice::from_raw_parts(sd.data, sd.size));
    }
    None
}
/// The container's own name for this track — `title` (Matroska and most formats), else `name`
/// (MP4's per-track `udta` name box, which is what FFmpeg's mov demuxer exposes it as).
///
/// Deliberately NOT `handler_name`: MP4 sets it to a constant per media type (`"SubtitleHandler"`,
/// `"SoundHandler"`), so it is the same string on every track and reading it would give the picker
/// nine identical sub-lines instead of none — the failure it exists to fix, dressed as a fix.
///
/// The returned `String` is a COPY. The entry points into the stream's dictionary, which
/// `avformat_close_input` frees.
unsafe fn stream_name(s: *mut AVStream) -> String {
    let dict = *((s as *const u8).add(OFF_STREAM_METADATA) as *const *const AVDictionary);
    if dict.is_null() {
        return String::new();
    }
    for key in [c"title", c"name"] {
        let e = av_dict_get(dict, key.as_ptr(), std::ptr::null(), 0);
        if e.is_null() || (*e).value.is_null() {
            continue;
        }
        // `to_string_lossy` rather than a strict decode: these are user-authored tags off a
        // stranger's file, and a mis-encoded byte must cost that character, not the whole name.
        let v = std::ffi::CStr::from_ptr((*e).value)
            .to_string_lossy()
            .trim()
            .to_string();
        if !v.is_empty() {
            return v;
        }
    }
    String::new()
}

/// Every track's container name, by TYPE, in file order — the two lists
/// `metadata::audio_ordinal` / `metadata::sub_render_ordinal` index into.
///
/// File order is the join, and it is the mapping this file already relies on twice
/// (`nth_audio_stream`, and the subtitle enumeration that feeds `desired_sub_idx`). It is worth
/// stating why that is sound rather than convenient: PMS lists a part's streams in container order
/// too, and both ordinal helpers skip the SIDECAR streams that exist only on the server — which is
/// exactly the set that is not in this file. So position N here and position N there name one
/// track, and a file with no tags simply yields empty strings in the right slots.
unsafe fn track_names(fmt: *mut AVFormatContext) -> (Vec<String>, Vec<String>) {
    let (mut audio, mut subs) = (Vec::new(), Vec::new());
    let streams = (*fmt).streams;
    for i in 0..(*fmt).nb_streams {
        let st = *streams.add(i as usize);
        match (*stream_codecpar(st)).codec_type {
            AVMEDIA_TYPE_AUDIO => audio.push(stream_name(st)),
            AVMEDIA_TYPE_SUBTITLE => subs.push(stream_name(st)),
            _ => {}
        }
    }
    (audio, subs)
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

// AVCodecContext width/height/pix_fmt USED to be poked here at 124/128/144, under a note claiming
// they had no AVOption. That was wrong — `video_size` (an IMAGE_SIZE option writing width AND
// height) and `pixel_format` are both in libavcodec's own option table — and Venc::open sets them
// by name now, so the constants are gone with the last read of them. Which is the point: all three
// move on webOS 5 (to 92/96/112), and a by-name setter does not care. `ci/ffabi-assert.c` still
// asserts them per major, so if anyone ever needs to poke them again the proven numbers are there.
// AVFrame (avutil 55, 32-bit ARM). pts sits at +104: a 4-byte pad at +100
// 8-aligns the int64 on ARM EABI (the classic AVFrame-on-ARM quirk).
const OFF_FRAME_DATA: usize = 0; // u8*[8] — the only one that does not move
#[cfg(target_pointer_width = "32")]
const OFF_FRAME_LINESIZE: usize = 32; // c_int[8]
#[cfg(target_pointer_width = "32")]
const OFF_FRAME_WIDTH: usize = 68;
#[cfg(target_pointer_width = "32")]
const OFF_FRAME_HEIGHT: usize = 72;
#[cfg(target_pointer_width = "32")]
const OFF_FRAME_FORMAT: usize = 80;
#[cfg(target_pointer_width = "32")]
const OFF_FRAME_PTS: usize = 96;
// 64-bit: `data` is eight POINTERS rather than eight 32-bit ones, so `linesize` doubles and
// everything after it follows. `pts` needs no pad here — it is already 8-aligned — which is the
// ARM EABI quirk described above seen from the side where it does not bite.
#[cfg(target_pointer_width = "64")]
const OFF_FRAME_LINESIZE: usize = 64;
#[cfg(target_pointer_width = "64")]
const OFF_FRAME_WIDTH: usize = 104;
#[cfg(target_pointer_width = "64")]
const OFF_FRAME_HEIGHT: usize = 108;
#[cfg(target_pointer_width = "64")]
const OFF_FRAME_FORMAT: usize = 116;
#[cfg(target_pointer_width = "64")]
const OFF_FRAME_PTS: usize = 136;
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
        ensure_registered(); // the file's ONE network-init guard
        if !SWS_OK.load(Ordering::Relaxed) {
            crate::log("venc: libswscale is not loaded (RELEASE build) — mpeg1 capture off");
            return None;
        }
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
                sink: Box::new(VencSink {
                    fd: -1,
                    failed: false,
                }),
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
                av_opt_set(
                    ctx as *mut c_void,
                    name.as_ptr() as *const c_char,
                    c.as_ptr(),
                    0,
                )
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
            // Runtime ABI self-check: the options set above must round-trip through the
            // AVCodecParameters model, or we stop before feeding frames.
            //
            // LIBRARY-ALLOCATED, never on the stack. `avcodec_parameters_from_context` begins by
            // memset-ing sizeof(AVCodecParameters) bytes, and that size is NOT stable across
            // FFmpeg majors — 136 on the n3.3 the TVs shipped, 176 on the 6.1 this app now
            // bundles. A stack copy sized by our own model is therefore a stack smash the moment
            // the two disagree, and it would be one written by the very check meant to catch ABI
            // drift. Allocating through the library makes the size the library's problem forever;
            // our model stays a read-only PREFIX, which is all it was ever used as.
            let par = avcodec_parameters_alloc();
            if par.is_null() {
                crate::log("venc: avcodec_parameters_alloc failed");
                return None;
            }
            let ok = avcodec_parameters_from_context(par, ctx) >= 0
                && (*par).width == w
                && (*par).height == h
                && (*par).format == fmt_yuv;
            if !ok {
                crate::log(&format!(
                    "venc: ABI self-check FAILED (par {}x{} fmt {} vs {}x{} fmt {}) — mpeg off",
                    (*par).width,
                    (*par).height,
                    (*par).format,
                    w,
                    h,
                    fmt_yuv
                ));
                avcodec_parameters_free(&mut { par });
                return None;
            }
            // The library owns par->extradata now, so free the whole thing rather than the
            // field: avcodec_parameters_free does both, and the old hand-free of just extradata
            // would have leaked the 176-byte struct.
            avcodec_parameters_free(&mut { par });
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
            v.sws = sws_getContext(
                w,
                h,
                fmt_rgba,
                w,
                h,
                fmt_nv12,
                SWS_BILINEAR,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if v.sws.is_null() {
                crate::log("venc: sws_getContext failed");
                return None;
            }
            // ---- muxer over custom AVIO ----
            let mut oc: *mut AVFormatContext = std::ptr::null_mut();
            let r = avformat_alloc_output_context2(
                &mut oc,
                std::ptr::null_mut(),
                b"mpegts\0".as_ptr() as *const c_char,
                std::ptr::null(),
            );
            if r < 0 || oc.is_null() {
                crate::log(&format!("venc: no mpegts muxer in this build ({r})"));
                return None;
            }
            v.oc = oc;
            let iobuf = av_malloc(32768) as *mut u8;
            if iobuf.is_null() {
                return None;
            }
            let avio = avio_alloc_context(
                iobuf,
                32768,
                1,
                v.sink.as_mut() as *mut VencSink as *mut c_void,
                None,
                Some(venc_write_cb),
                None,
            );
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
            crate::log(&format!(
                "venc: mpeg1/ts up {}x{} @{}bps (st_tb {}/{})",
                w, h, bitrate_bps, v.st_tb.num, v.st_tb.den
            ));
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
            let dst: [*mut u8; 4] = [
                *fdata,
                self.nv12_uv.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ];
            let dst_stride: [c_int; 4] = [*fls, self.w, 0, 0];
            sws_scale(
                self.sws,
                src.as_ptr(),
                src_stride.as_ptr(),
                0,
                self.h,
                dst.as_ptr(),
                dst_stride.as_ptr(),
            );
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
                crate::log(&format!(
                    "venc: {} frm, sws {:.1}ms enc {:.1}ms avg",
                    self.t_n,
                    self.t_sws_us as f32 / self.t_n as f32 / 1000.0,
                    self.t_enc_us as f32 / self.t_n as f32 / 1000.0
                ));
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
        crate::player::log(&format!(
            "ff: no image-sub decoder for codec_id={}",
            (*cp).codec_id
        ));
        return std::ptr::null_mut();
    }
    let ctx = avcodec_alloc_context3(codec);
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    if avcodec_parameters_to_context(ctx, cp) < 0
        || avcodec_open2(ctx, codec, std::ptr::null_mut()) < 0
    {
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
    if idx.is_null()
        || pal.is_null()
        || w <= 0
        || h <= 0
        || stride < w
        || w > MAX_SIDE
        || h > MAX_SIDE
    {
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
unsafe fn decode_bitmap_cue(
    dec: *mut AVCodecContext,
    pkt: *mut AVPacket,
    track: c_int,
    st: *mut AVStream,
) {
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
/// One-time `avformat_network_init`. Was also the home of `av_register_all`, which FFmpeg 4.0
/// made a no-op and 5.0 deleted — registration is automatic now, and the bundled build is 9.0.
fn ensure_registered() {
    REGISTER.call_once(|| unsafe {
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
    // The bundled FFmpeg lives beside the binary, which is on no library search path — so it is
    // opened by ABSOLUTE PATH. That is not a convenience: webOS 11.2.0 ships FFmpeg 6 itself, and
    // a bare SONAME there could open the television's copy instead of ours.
    let dir = Some(crate::paths::app_dir());

    // DEPENDENCY ORDER, and it is load-bearing. libavformat NEEDs libavcodec NEEDs libavutil by
    // SONAME, and these libraries carry no rpath (FFmpeg's configure evals its flags, so
    // -Wl,-rpath,$ORIGIN does not survive — see ci/build-ffmpeg.sh). Loading a dependency FIRST
    // with RTLD_GLOBAL puts it in the global scope under its SONAME, so when the next library
    // names it the loader finds the one we just opened rather than searching the system paths and
    // finding the TV's. Open libavformat first and that guarantee is gone.
    let mut ok = true;
    for (what, verdict) in [
        ("avutil", avutil::load(dir)),
        ("avcodec", avcodec::load(dir)),
        ("avformat", avformat::load(dir)),
    ] {
        match verdict {
            crate::dynlib::Loaded::Ok(soname) => {
                crate::log(&format!("ff: bound {what} -> {soname}"))
            }
            crate::dynlib::Loaded::NoLibrary => {
                ok = false;
                crate::log(&format!(
                    "ff: {what} is MISSING from the app directory — the bundled FFmpeg did not \
                     deploy. Playback will refuse; reinstall the package."
                ));
            }
            crate::dynlib::Loaded::Incomplete(soname, n) => {
                ok = false;
                crate::log(&format!(
                    "ff: {soname} is missing {n} symbol(s) we need — named above"
                ));
            }
        }
    }
    // swscale is NOT required, and treating it as required broke the RELEASE build entirely:
    // `RELEASE=1` drops it from the package (only the dev capture stream's RGBA->YUV conversion
    // uses it), so an all-or-nothing loop over four libraries would have found three, reported
    // failure, and refused to play anything — in the configuration users actually receive, and in
    // no configuration ever tested here.
    match swscale::load(dir) {
        crate::dynlib::Loaded::Ok(soname) => {
            SWS_OK.store(true, Ordering::Relaxed);
            crate::log(&format!("ff: bound swscale -> {soname}"));
        }
        _ => {
            crate::log("ff: no swscale (expected in a RELEASE build) — dev capture JPEG/MPEG1 off")
        }
    }
    ok
}

/// Did the bundled FFmpeg load and match the ABI table? The diagnostics read-out's first question
/// about the demuxer: false means every playback will refuse before it opens a socket, which from
/// the outside looks identical to a stall.
fn abi_ok() -> bool {
    ABI_OK.load(std::sync::atomic::Ordering::Relaxed)
}

/// Did the CURRENT demux session hand over a single video AU? Distinguishes "the demuxer produced
/// nothing" from "it produced AUs that never reached the decoder" — two stalls that present
/// identically as a spinner. See [`PUSHED_ANY`].
pub(crate) fn pushed_any() -> bool {
    PUSHED_ANY.load(std::sync::atomic::Ordering::Relaxed)
}

/// The three FFmpeg majors actually bound, for the read-out's build line. Zeroes before [`boot`]
/// has run or when the libraries did not load.
pub(crate) fn majors() -> (u32, u32, u32) {
    if !abi_ok() {
        return (0, 0, 0);
    }
    unsafe {
        (
            avformat_version() >> 16,
            avcodec_version() >> 16,
            avutil_version() >> 16,
        )
    }
}

/// Boot smoke test + optional ABI probe. Called once at startup.
pub(crate) fn boot() {
    // The https media transport's own table, resolved here so the `dlopen` and its log line land
    // on the MAIN thread at start-up rather than inside a demux worker — and after `app.rs` has
    // run `net::global_init`, which is what makes `curl_global_init` main-thread-only as its doc
    // requires. Independent of FFmpeg: a set with no libcurl multi still demuxes local samples,
    // and a set with no FFmpeg still signs in.
    crate::curlio::boot();
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
        // THE MAJORS ARE THE ABI. Everything below reads FFmpeg structs at offsets that are fixed
        // per major: FFmpeg guarantees layout only within one, and majors reorder.
        //
        // One version, checked against the one we built for. This used to SELECT between two
        // offset tables because the app read the television's FFmpeg and every webOS release
        // shipped a different one; bundling made that a single equality. The refusal still
        // matters: a wrong offset does not fail, it succeeds with garbage. Reading `codecpar` at
        // the n3.3 offset on a modern AVStream lands hundreds of bytes past the struct and
        // dereferences whatever it finds, on a device with no debugger.
        // The version we BUILT against, not a version we hope to find. These libraries ship
        // inside the package under a -plx SONAME, so a mismatch means the deployed payload is
        // stale or someone put a different file there — not a firmware difference.
        let bad = (fmt >> 16, cod >> 16, utl >> 16) != (63, 63, 61);
        ABI_OK.store(!bad, Ordering::Relaxed);
        if bad {
            crate::log(
                "ff: BUNDLED FFmpeg is not the one this build expects (want avformat 63 / \
                 avcodec 63 / avutil 61) — the app directory holds a stale or foreign \
                 libav*-plx; refusing to demux",
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
        let r = avformat_open_input(
            &mut fmt,
            cu.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
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
            fmt_duration(fmt)
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

/// **Which transport is under the AVIO.** The two are not interchangeable and the choice is made
/// once, in `demux`, from the part URL's SCHEME — never guessed per call.
///
/// * [`Src::Socket`] is the original and the default: `stream.rs`'s raw TCP socket wrapping the
///   ENGINE-owned `HttpStream` — cleartext, numeric address, seeking by closing and re-opening
///   with a byte `Range`. Its behaviour here is byte for byte what it was before this enum
///   existed.
/// * [`Src::Curl`] is the https path ([`crate::curlio`]), which the demux thread owns outright.
///   `ff.rs` never learns curl-multi mechanics: all it can ask is `read`/`seek`/`size`/`status`/
///   `abort`, deliberately the same five questions the socket answers.
enum Src {
    Socket {
        /// The Engine's stream — NOT owned here. `player::engine` allocates it, publishes it in
        /// `SHARED.hs_ptr`, `http_shutdown`s it at teardown and closes it after the join.
        hs: *mut HttpStream,
        host: CString,
        port: c_int,
        path: CString,
    },
    /// Owned by this state, and so by the demux thread — which is why teardown reaches it through
    /// `curlio`'s registry instead of a pointer the engine holds. `curlio`'s module doc says why
    /// that is not the accident it looks like.
    Curl(Box<crate::curlio::CurlSource>),
}

/// **The live-transfer terminal rule.** A partial response is right-censored evidence.  PMS may
/// open a sized HLS response, deliver a prefix, wait for its JIT encoder (`segmentWait` in the
/// server's own SessionReport), then burst the remainder.  Consequently neither
/// `prefix_bytes / prefix_time` nor `Content-Length - prefix_bytes` identifies the unseen
/// acquisition time.  The former attained-rate projection falsely abandoned sustainable 4K
/// responses on the remote PMS.
///
/// There is one coefficient-free fact available before completion: the playable reserve has
/// actually reached zero.  The main thread observes that physical boundary and holds Starfish's
/// clock; this guard observes the same hold (or the equivalent playhead spend) at the next AVIO
/// callback.  Above the ladder floor it abandons the still-incomplete object so the controller can
/// fetch a smaller one.  At the floor it leaves the only useful response alive.  No prefix becomes
/// a capacity sample in either direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StallGuard {
    /// The playable reserve when this fetch began, in ms of media. `None` is never stored — an
    /// unknowable reserve (`BufferSnapshot::buffered_ms`'s `None`, the audio lane silent after an
    /// open or a seek) arms no guard at all, because the comparison has no left-hand side.
    reserve_ms_at_start: i64,
    /// Playhead at the fetch boundary.  Wall time is deliberately absent: queued media is spent
    /// by presentation, not by a blocked request, pause, seek or server wait.
    playhead_at_start_ns: i64,
    action: StallAction,
}

/// What happens after the same in-flight conservation inequality is crossed. Above the floor a
/// smaller object exists, so abandoning the response releases the controller to fetch it. At the
/// floor abandoning can only re-request the same bytes; keep that response alive and hold the A/V
/// clock instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StallAction {
    AbortFetch,
    HoldClock,
}

impl StallGuard {
    /// **Arm the guard, or decline to.** A reserve of zero AT THE START of a fetch is not an
    /// emergency — it is what every session looks like before its first segment has been
    /// delivered, and the rule's premise is that continuing projects a stall the abort could
    /// avoid. With no picture yet there is nothing to stall and the abort has nothing to buy: it
    /// simply abandons the fetch, pushes the segment back, and abandons it again.
    ///
    /// **That is not hypothetical.** Armed without this gate, device-measured 2026-08-28:
    /// `abr: stall abort seq=0 bytes=2896 of 0ms reserve` — playback never started at all, one
    /// segment fetched, the video plane never bound. It is the same shape as the plan's I3
    /// (the first-segment false downshift): a predicate keyed on the reserve, evaluated at the one
    /// moment the reserve is empty for a reason that is not failure.
    ///
    /// Mid-playback a genuinely drained reserve declines the guard too, and that is the
    /// conservative direction rather than a gap: by then the picture has already stopped, the
    /// abort can no longer prevent anything, and `starving()` is the predicate that owns it.
    fn arm(reserve_ms_at_start: i64) -> Option<Self> {
        (reserve_ms_at_start > 0).then(|| Self {
            reserve_ms_at_start,
            playhead_at_start_ns: crate::player::playpos_ns(),
            action: StallAction::AbortFetch,
        })
    }

    /// At the terminal actuator the same observed boundary may stop presentation, but it must not
    /// discard the only response capable of producing more media.
    fn arm_clock_hold(reserve_ms_at_start: i64) -> Option<Self> {
        Self::arm(reserve_ms_at_start).map(|mut guard| {
            guard.action = StallAction::HoldClock;
            guard
        })
    }

    fn aborts_fetch(self) -> bool {
        self.action == StallAction::AbortFetch
    }

    /// Has the fetch crossed an OBSERVED terminal boundary while bytes are known to remain?
    /// `terminal_hold_started` is the main thread's terminal observation — an exact arithmetic
    /// `B=0`, or a presentation clock that has physically stopped (`pump::native_clock_stopped`).
    /// The playhead form is the same arithmetic sampled by this worker, and on this pipeline it is
    /// a bound rather than a trigger: `reserve_ms_at_start` is measured to the demuxed tail, and
    /// the television parks the clock a few hundred ms short of that tail (the last fed access
    /// units cannot present until the ones behind them arrive), so `spent_ms` stops accruing just
    /// below the reserve and the hold has to come from the main thread. Device-measured
    /// 2026-09-02 (`pipe_abr_down_collapse`): reserve 5668 ms, playhead spent 5410 ms, then 77 s
    /// of `vtick=0` with neither branch firing. Neither branch predicts an unseen byte.
    fn should_abort(&self, bytes_remaining: i64, terminal_hold_started: bool) -> bool {
        if bytes_remaining <= 0 {
            return false;
        }
        let spent_ms = crate::player::playpos_ns()
            .saturating_sub(self.playhead_at_start_ns)
            .max(0)
            / 1_000_000;
        terminal_hold_started || spent_ms >= self.reserve_ms_at_start
    }
}

/// An already-held clock is a recovery state: the active response is rebuilding reserve and may
/// not be abandoned by the boundary that has already fired. Kept pure at the call boundary so a
/// regression cannot silently turn B=0 into an abort loop again.
fn arm_active_stall_guard(
    reserve_ms: Option<i64>,
    at_floor: bool,
    already_held: bool,
) -> Option<StallGuard> {
    if already_held {
        return None;
    }
    reserve_ms.and_then(|reserve_ms| {
        if at_floor {
            StallGuard::arm_clock_hold(reserve_ms)
        } else {
            StallGuard::arm(reserve_ms)
        }
    })
}

/// A rollback-reserve deadline is monotone in one direction: a floor downshift that reaches an
/// actual internal rebuffer may stop protecting the old cursor, but a later user Resume cannot
/// make that already-spent reserve exist again. One instance follows the whole segment transaction
/// through open, wait, probe, body and final validation; promotion is latched as an unarmed clock.
#[derive(Clone, Copy, Debug)]
struct ReserveDeadlineState {
    clock: ReserveDeadlineClock,
    yields_to_rebuffer: bool,
    hold_epoch_at_start: u64,
    expired: bool,
}

#[derive(Clone, Copy, Debug)]
enum ReserveDeadlineClock {
    Wall(Option<std::time::Instant>),
    Playhead {
        started_ns: i64,
        budget: std::time::Duration,
    },
    /// A bounded recovery transfer spends every elapsed interval except native-accepted user
    /// Pause. Unlike a playhead clock it therefore continues through involuntary starvation and
    /// the internal rebuffer hold; unlike a wall deadline it preserves Pause exactly, including a
    /// complete Pause->Resume cycle hidden inside one blocked network call.
    UnpausedElapsed {
        started_at: std::time::Instant,
        paused_at_start: std::time::Duration,
        budget: std::time::Duration,
    },
}

impl ReserveDeadlineState {
    fn new(at: Option<std::time::Instant>, yields_to_rebuffer: bool) -> Self {
        Self {
            clock: ReserveDeadlineClock::Wall(at),
            yields_to_rebuffer,
            hold_epoch_at_start: SHARED.hls_internal_hold_epoch.load(Ordering::Acquire),
            expired: false,
        }
    }

    /// A media-reserve deadline spends presentation time, not wall time. User Pause, an internal
    /// rebuffer hold and a genuinely starved native clock all freeze the same physical balance.
    fn from_playhead(budget: std::time::Duration, yields_to_rebuffer: bool) -> Self {
        Self::from_playhead_at(
            crate::player::playpos_ns().max(0),
            budget,
            yields_to_rebuffer,
        )
    }

    fn from_playhead_at(
        started_ns: i64,
        budget: std::time::Duration,
        yields_to_rebuffer: bool,
    ) -> Self {
        Self {
            clock: ReserveDeadlineClock::Playhead {
                started_ns: started_ns.max(0),
                budget,
            },
            yields_to_rebuffer,
            hold_epoch_at_start: SHARED.hls_internal_hold_epoch.load(Ordering::Acquire),
            expired: false,
        }
    }

    fn from_unpaused_elapsed(budget: std::time::Duration, yields_to_rebuffer: bool) -> Self {
        let (started_at, paused_at_start) = crate::player::TX.pause_clock_sample();
        Self {
            clock: ReserveDeadlineClock::UnpausedElapsed {
                started_at,
                paused_at_start,
                budget,
            },
            yields_to_rebuffer,
            hold_epoch_at_start: SHARED.hls_internal_hold_epoch.load(Ordering::Acquire),
            expired: false,
        }
    }

    /// Remaining reserve in the clock's own units. Unlike [`Self::active`], this deliberately
    /// still returns the balance while the user is paused: callers use it to carry one fixed
    /// end-to-end exploration budget from the control plane into the media leg without turning a
    /// pause into either spent reserve or a newly granted budget.
    fn remaining(&self) -> Option<std::time::Duration> {
        match self.clock {
            ReserveDeadlineClock::Wall(at) => {
                at.map(|at| at.saturating_duration_since(std::time::Instant::now()))
            }
            ReserveDeadlineClock::Playhead { started_ns, budget } => {
                let spent_ns = crate::player::playpos_ns()
                    .max(0)
                    .saturating_sub(started_ns);
                let spent =
                    std::time::Duration::from_nanos(u64::try_from(spent_ns).unwrap_or(u64::MAX));
                Some(budget.saturating_sub(spent))
            }
            ReserveDeadlineClock::UnpausedElapsed {
                started_at,
                paused_at_start,
                budget,
            } => {
                let (now, paused_now) = crate::player::TX.pause_clock_sample();
                let elapsed = now.saturating_duration_since(started_at);
                let paused = paused_now.saturating_sub(paused_at_start);
                Some(budget.saturating_sub(elapsed.saturating_sub(paused)))
            }
        }
    }

    fn release_to_rebuffer(&mut self, rebuffering: bool) {
        let hold_happened =
            SHARED.hls_internal_hold_epoch.load(Ordering::Acquire) != self.hold_epoch_at_start;
        if self.yields_to_rebuffer && (rebuffering || hold_happened) {
            self.clock = ReserveDeadlineClock::Wall(None);
        }
    }

    /// Whether the conservation clock itself is spent. A wall projection returned by `active`
    /// is only a wake-up mechanism; it is never evidence for a playhead-funded budget, because
    /// wall time may pass while the native presentation clock is stopped.
    fn clock_is_spent(&self) -> bool {
        match self.clock {
            ReserveDeadlineClock::Wall(at) => at.is_some_and(|at| std::time::Instant::now() >= at),
            ReserveDeadlineClock::Playhead { started_ns, budget } => {
                let spent_ns = crate::player::playpos_ns()
                    .max(0)
                    .saturating_sub(started_ns);
                let spent =
                    std::time::Duration::from_nanos(u64::try_from(spent_ns).unwrap_or(u64::MAX));
                spent >= budget
            }
            ReserveDeadlineClock::UnpausedElapsed { .. } => {
                self.remaining() == Some(std::time::Duration::ZERO)
            }
        }
    }

    fn active(&mut self, rebuffering: bool) -> Option<std::time::Instant> {
        self.release_to_rebuffer(rebuffering);
        match self.clock {
            ReserveDeadlineClock::Wall(at) => at,
            ReserveDeadlineClock::Playhead { .. }
            | ReserveDeadlineClock::UnpausedElapsed { .. } => {
                let mut left = self.remaining().unwrap_or(std::time::Duration::ZERO);
                if crate::player::TX.paused.load(Ordering::Acquire) {
                    // Pause is published only after the native call succeeds, but the final
                    // playhead sample can land between our first read and that flag. Re-read once
                    // after observing Pause so equality cannot be hidden by the state change.
                    left = self.remaining().unwrap_or(std::time::Duration::ZERO);
                }
                // This wall instant schedules a re-check; it never spends presentation reserve.
                // Keeping that wake armed while paused closes Pause-at-issue -> Resume-in-flight:
                // if the playhead starts moving again, the existing wait still wakes no later
                // than the remaining one-speed balance. If Pause remains, retrospective
                // classification sees an unchanged playhead and retries without renewing the
                // independent transport watchdog.
                if left.is_zero() {
                    return Some(std::time::Instant::now());
                }
                std::time::Instant::now().checked_add(left)
            }
        }
    }

    fn expire_if_due(&mut self, rebuffering: bool) -> bool {
        self.release_to_rebuffer(rebuffering);
        if self.expired || self.clock_is_spent() {
            self.expired = true;
            true
        } else {
            false
        }
    }

    /// Record the result of a read started with `attempted_deadline`. `false` means that wall wake
    /// did not spend the clock which owns the reserve. Pause or a naturally stopped playhead may
    /// have made the projection stale; a floor rebuffer hold may instead have released that clock.
    /// The caller retries with whatever reserve remains armed, composed with the unchanged
    /// transport-liveness epoch.
    fn note_transport_deadline(
        &mut self,
        _attempted_deadline: Option<std::time::Instant>,
        rebuffering: bool,
    ) -> bool {
        self.release_to_rebuffer(rebuffering);
        // The transport waited on a wall projection. Classify the result only from the clock that
        // owns the budget: Δplayhead >= E for a media reserve, or the actual wall boundary for a
        // legacy wall state. Nanoseconds spent between two `Instant::now()` calls cannot turn a
        // positive presentation balance into censored evidence.
        if self.expired || self.clock_is_spent() {
            self.expired = true;
            true
        } else {
            false
        }
    }
}

/// Independent transport liveness for one blocking HTTP leg. Reserve is presentation-funded and
/// can freeze; liveness is wall-clock inactivity and cannot. The effective wake is their minimum,
/// but only the clock that actually reached its own boundary classifies the result. Progress
/// resets liveness exactly as `SO_RCVTIMEO`/curl low-speed do; an obsolete reserve retry does not.
#[derive(Clone, Copy, Debug)]
struct TransportWatchdog {
    inactivity: std::time::Duration,
    deadline: std::time::Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockingDeadlineOwner {
    /// Reserve wins equality: when both clocks name the same instant and presentation really
    /// spends its balance, the completed observation is exactly reserve-censored.
    Reserve,
    Transport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BlockingDeadline {
    at: std::time::Instant,
    owner: BlockingDeadlineOwner,
}

impl TransportWatchdog {
    fn for_origin(origin: &crate::plex::Origin) -> Self {
        let inactivity = if origin.is_tls() {
            crate::curlio::media_stall_budget()
        } else {
            crate::stream::media_stall_budget()
        };
        Self::with_inactivity(inactivity)
    }

    fn with_inactivity(inactivity: std::time::Duration) -> Self {
        Self {
            inactivity,
            deadline: std::time::Instant::now()
                .checked_add(inactivity)
                .unwrap_or_else(std::time::Instant::now),
        }
    }

    fn reset(&mut self) {
        self.deadline = std::time::Instant::now()
            .checked_add(self.inactivity)
            .unwrap_or_else(std::time::Instant::now);
    }

    fn effective(&self, reserve: Option<std::time::Instant>) -> BlockingDeadline {
        match reserve {
            Some(at) if at <= self.deadline => BlockingDeadline {
                at,
                owner: BlockingDeadlineOwner::Reserve,
            },
            _ => BlockingDeadline {
                at: self.deadline,
                owner: BlockingDeadlineOwner::Transport,
            },
        }
    }

    fn expired(&self) -> bool {
        std::time::Instant::now() >= self.deadline
    }
}

/// Facts captured immediately when a typed deadline sentinel returns. Keeping the observation
/// separate from its later use prevents scheduler delay, AQ locking or native-clock catch-up from
/// relabelling the boundary which actually ended this I/O operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BlockingWake {
    owner: BlockingDeadlineOwner,
    reserve_spent: bool,
    transport_due: bool,
}

fn observe_hls_deadline(
    reserve: Option<&mut ReserveDeadlineState>,
    attempted: BlockingDeadline,
    watchdog: &TransportWatchdog,
    rebuffering: bool,
) -> BlockingWake {
    let reserve_spent = attempted.owner == BlockingDeadlineOwner::Reserve
        && reserve.is_some_and(|reserve| {
            reserve.note_transport_deadline(Some(attempted.at), rebuffering)
        });
    BlockingWake {
        owner: attempted.owner,
        reserve_spent,
        transport_due: watchdog.expired(),
    }
}

/// Classify only the captured boundary cause. A transport-owned wake stays a circumstance even
/// if reserve is spent later; that later fact can gate the next I/O but cannot turn an RST/stall
/// into censored rung evidence. Reserve wins an issued equality, but only when its owning clock
/// was actually spent at wake; a stale reserve projection then yields to due liveness.
fn classify_hls_deadline(wake: BlockingWake) -> Option<HlsExit> {
    match wake.owner {
        BlockingDeadlineOwner::Transport if wake.transport_due => {
            Some(HlsExit::Failed("HLS transport stalled"))
        }
        BlockingDeadlineOwner::Reserve if wake.reserve_spent => Some(HlsExit::PrimeExpired),
        BlockingDeadlineOwner::Reserve if wake.transport_due => {
            Some(HlsExit::Failed("HLS transport stalled"))
        }
        _ => None,
    }
}

/// Preserve the one exploration clock across its media leg. An upshift starts its playhead clock
/// before PMS registration and the playlist requests; replacing it here with their last wall
/// snapshot made a user Pause expire unspent playable reserve. A downshift's formula is known only
/// after its actual candidate EXTINF and fresh reserve are known, so its media-only clock starts at
/// this boundary and spends unpaused elapsed time: internal starvation is recovery cost, accepted
/// user Pause is not.
fn candidate_reserve_deadline(
    exploration: Option<ReserveDeadlineState>,
    media_budget: Option<std::time::Duration>,
    yields_to_rebuffer: bool,
    started_ns: i64,
    direction: crate::abr::Direction,
) -> ReserveDeadlineState {
    exploration.unwrap_or_else(|| {
        media_budget.map_or_else(
            || ReserveDeadlineState::new(None, yields_to_rebuffer),
            |budget| match direction {
                crate::abr::Direction::Up => {
                    ReserveDeadlineState::from_playhead_at(started_ns, budget, yields_to_rebuffer)
                }
                crate::abr::Direction::Down => {
                    ReserveDeadlineState::from_unpaused_elapsed(budget, yields_to_rebuffer)
                }
            },
        )
    })
}

fn exploration_snapshot(
    exploration: &mut Option<ReserveDeadlineState>,
) -> Option<std::time::Instant> {
    exploration
        .as_mut()
        .and_then(|reserve| reserve.active(false))
}

fn exploration_expired(exploration: &mut Option<ReserveDeadlineState>) -> bool {
    exploration
        .as_mut()
        .is_some_and(|reserve| reserve.expire_if_due(false))
}

/// Whether a blocking leg really consumed its playhead-funded exploration reserve. `false` means
/// the leg returned on an obsolete wall-clock snapshot after Pause or a naturally stopped native
/// clock moved the physical boundary; idempotent HTTP legs may retry with a fresh snapshot.
fn exploration_timeout_is_final(
    exploration: &mut Option<ReserveDeadlineState>,
    attempted: Option<std::time::Instant>,
) -> bool {
    exploration.as_mut().map_or(true, |reserve| {
        reserve.note_transport_deadline(attempted, false)
    })
}

/// AVIO backing state: wraps the demux transport so libavformat reads through it and can seek by
/// byte offset. Boxed so its address is stable for the C callbacks.
struct AvioState {
    src: Src,
    aq: *mut AuQueue,
    off: i64,
    size: i64,
    /// A callback saw a curl I/O failure during the current enclosing libavformat operation.
    /// FFmpeg may heal it through `seek_cb`; only an operation that still returns failure may
    /// publish it to the player.
    io_failed: bool,
    /// Time spent inside successful body reads only. Request setup, PMS JIT production, TTFB and
    /// demux/probe work intentionally stay out of this network-rate clock and remain in the
    /// enclosing segment's total acquisition clock.
    body_active_us: u64,
    body_bytes: u64,
    /// When the FIRST body byte reached the demuxer. Together with the open above it this splits
    /// acquisition into connect / server-think / transfer, which is the only way to see PMS
    /// just-in-time production as its own term: for a JIT encoder the wait before the first byte
    /// IS the production cost, and it is otherwise indistinguishable from a slow link.
    first_byte_at: Option<std::time::Instant>,
    /// Only ABR candidate segments carry a reserve deadline. The active movie and every progressive
    /// stream retain their transport's normal stall budgets. A floor downshift may promote to
    /// unbounded-by-reserve while it is in flight; the state latches that transition.
    reserve_deadline: ReserveDeadlineState,
    /// Independent wall-clock inactivity for HLS media. Its deadline is composed with the
    /// reserve projection on every blocking read, but only actual bytes reset it. Progressive
    /// sources keep `None` and use their transport's native liveness directly.
    transport_watchdog: Option<TransportWatchdog>,
    /// Armed for the ACTIVE cursor whenever its reserve is knowable and the internal clock is not
    /// already held. Its action distinguishes an abort above the ladder floor from a clock-only
    /// hold at the terminal actuator. Candidate transactions use `reserve_deadline` instead: an
    /// ordinary candidate owns a playhead-funded clock projected onto each blocking wait, while a
    /// terminal-floor recovery may omit that reserve clock. A partial PMS response remains
    /// right-censored and cannot prove an earlier completion time from its prefix rate.
    stall: Option<StallGuard>,
    stall_aborted: bool,
}

impl AvioState {
    /// **Fold the AU lane's abort into the transport's own.**
    ///
    /// There are two abort signals in the media path and they arrive by different routes. The AU
    /// lane's flag (`aq_is_aborted`) is what these callbacks have always checked on entry, and it
    /// is set by every stopper — teardown, and `start_bufferfeed`'s early return when the media
    /// thread will not spawn. `curlio`'s wake pipe is what reaches a thread already BLOCKED inside
    /// `curl_multi_wait`, which the AU flag cannot do; teardown fires it separately.
    ///
    /// This is the join between them, in the one place both are visible: once the lane is aborted,
    /// the curl source is latched too, so it refuses on its own terms afterwards rather than
    /// depending on every future caller re-checking the lane. The socket source needs no
    /// equivalent — closing it IS the latch, and the engine owns that.
    fn latch_abort(&self) {
        if let Src::Curl(cs) = &self.src {
            cs.abort();
        }
    }
}

/// Settle one enclosing FFmpeg operation against the exact callback state it accumulated.
///
/// Abort is checked by the caller immediately after the native call and therefore has priority
/// outside this helper. Of the remaining transaction terminals, deliberate active-stream stall
/// and a spent reserve outrank generic I/O; callback I/O is published only when FFmpeg itself did
/// not recover the operation. Using this one classifier for open, stream discovery and frame read
/// prevents the same callback event from changing meaning with the FFmpeg phase it happened in.
fn classify_hls_avio_operation(
    state: &AvioState,
    operation_failed: bool,
    request_started: std::time::Instant,
    audio_expected: bool,
) -> Option<HlsExit> {
    let stall = state.stall_aborted.then(|| SegmentTransfer {
        bytes: state.body_bytes,
        active_us: state.body_active_us,
        total_us: request_started.elapsed().as_micros().max(1) as u64,
        audio_expected,
    });
    classify_hls_avio_facts(
        stall,
        state.reserve_deadline.expired,
        state.io_failed,
        operation_failed,
    )
}

/// Pure terminal reduction for one completed native operation. The callback records facts while
/// libavformat owns the thread; this reducer is the state-machine edge taken when control returns.
/// Keeping the priority here makes `open`, `find_stream_info` and `read_frame` observe the same
/// transition table, and lets the table be tested without constructing an FFmpeg AVIO context.
fn classify_hls_avio_facts(
    stall: Option<SegmentTransfer>,
    reserve_expired: bool,
    io_failed: bool,
    operation_failed: bool,
) -> Option<HlsExit> {
    if let Some(transfer) = stall {
        Some(HlsExit::StallAbort(transfer))
    } else if reserve_expired {
        Some(HlsExit::PrimeExpired)
    } else if operation_failed && io_failed {
        Some(HlsExit::Failed("segment body transport failed"))
    } else {
        None
    }
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
        loop {
            // interrupt: bail out of a blocked read on teardown (aborted) only. A seek does NOT
            // interrupt the read — the demux thread services it itself between two av_read_frame
            // calls (see the read loop), so there is nothing to unblock and nothing to race.
            if crate::aq::aq_is_aborted(s.aq) {
                s.latch_abort();
                return AVERROR_EOF;
            }
            let rebuffering = SHARED.hls_rebuffering.load(Ordering::Acquire);
            if s.reserve_deadline.expire_if_due(rebuffering) {
                return AVERROR_EOF;
            }
            let reserve_snapshot = s.reserve_deadline.active(rebuffering);
            let blocking_deadline = s
                .transport_watchdog
                .map(|watchdog| watchdog.effective(reserve_snapshot))
                .or_else(|| {
                    reserve_snapshot.map(|at| BlockingDeadline {
                        at,
                        owner: BlockingDeadlineOwner::Reserve,
                    })
                });
            let live_deadline = blocking_deadline.map(|deadline| deadline.at);
            // The abort rule, evaluated where the numbers are. See `StallGuard`.
            if let Some(guard) = s.stall {
                if guard.should_abort(s.size - s.off, rebuffering) {
                    // The playable reserve has actually reached its terminal boundary. Always ask
                    // the main thread to hold Starfish's A/V clock (it may already have done so on
                    // its exact B=0 sample). Above the ladder floor, abandon this still-incomplete
                    // object so the controller can fetch a smaller one; at the floor leave the
                    // only useful response alive and disarm only the one-shot notification.
                    SHARED.hls_rebuffer_request_tail_ns.store(
                        SHARED.hls_video_tail_ns.load(Ordering::Acquire),
                        Ordering::Release,
                    );
                    SHARED.hls_rebuffer_requested.store(true, Ordering::Release);
                    if guard.aborts_fetch() {
                        s.stall_aborted = true;
                        return AVERROR_EOF;
                    }
                    s.stall = None;
                }
            }
            // Both sources use the same three-way return — >0 bytes, 0 clean end, <0 error — so
            // the EOF decision below stays one branch rather than one per transport.
            let read_started = std::time::Instant::now();
            let r = match &mut s.src {
                Src::Socket { hs, .. } => {
                    crate::stream::http_read_until(*hs, dst as *mut c_uchar, n, live_deadline)
                }
                // The null/length guard `stream::http_read` does for itself.
                Src::Curl(_) if dst.is_null() || n <= 0 => 0,
                Src::Curl(cs) => cs.read_until(
                    std::slice::from_raw_parts_mut(dst, n as usize),
                    live_deadline,
                ),
            };
            let wake =
                if r == crate::stream::HTTP_READ_DEADLINE || r == crate::curlio::READ_DEADLINE {
                    let current_rebuffering = SHARED.hls_rebuffering.load(Ordering::Acquire);
                    match (blocking_deadline, s.transport_watchdog.as_ref()) {
                        (Some(attempted), Some(watchdog)) => Some(observe_hls_deadline(
                            Some(&mut s.reserve_deadline),
                            attempted,
                            watchdog,
                            current_rebuffering,
                        )),
                        (Some(attempted), None) => Some(BlockingWake {
                            owner: attempted.owner,
                            reserve_spent: s
                                .reserve_deadline
                                .note_transport_deadline(Some(attempted.at), current_rebuffering),
                            transport_due: false,
                        }),
                        _ => None,
                    }
                } else {
                    None
                };
            // Teardown may have arrived while the transport was blocked. Its wake commonly
            // surfaces as socket EOF or curl failure; classify from the owning AU abort flag
            // before either transport result can become an active-stream error.
            if crate::aq::aq_is_aborted(s.aq) {
                s.latch_abort();
                return AVERROR_EOF;
            }
            if let Some(wake) = wake {
                match classify_hls_deadline(wake) {
                    Some(HlsExit::PrimeExpired) => return AVERROR_EOF,
                    Some(_) => {
                        s.io_failed = true;
                        return AVERROR_IO;
                    }
                    None => {}
                }
                // A stale reserve projection is neither evidence nor progress. Retry in this
                // frame (not recursively) with the same inactivity epoch and a fresh projection.
                continue;
            }
            if r < 0 {
                // Teardown was handled above through the AU flag and remains EOF. A negative
                // result here is therefore a real transport/range failure.
                s.io_failed = true;
                return AVERROR_IO;
            }
            if r == 0 {
                return AVERROR_EOF;
            }
            if let Some(watchdog) = &mut s.transport_watchdog {
                watchdog.reset();
            }
            s.body_active_us = s
                .body_active_us
                .saturating_add(read_started.elapsed().as_micros().max(1) as u64);
            if s.first_byte_at.is_none() {
                s.first_byte_at = Some(std::time::Instant::now());
            }
            s.body_bytes = s.body_bytes.saturating_add(r as u64);
            s.off += r as i64;
            SHARED.dg_net_rx.fetch_add(r as i64, Ordering::Relaxed);
            return r;
        }
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
            s.latch_abort();
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
        let ok = match &mut s.src {
            Src::Socket {
                hs,
                host,
                port,
                path,
            } => {
                crate::stream::http_close(*hs);
                let range =
                    CString::new(format!("Range: bytes={}-\r\n", target)).unwrap_or_default();
                crate::stream::http_open(
                    *hs,
                    host.as_ptr(),
                    *port,
                    path.as_ptr(),
                    range.as_ptr(),
                    "GET",
                ) == 0
            }
            // `curlio` REFUSES a Range the server answered with a 200, where `stream.rs` accepts
            // any 2xx. That is the one behavioural difference between these two arms, and it is
            // deliberate: bytes from the head of the file, delivered as though they were the bytes
            // at `target`, are corruption that looks like success.
            Src::Curl(cs) => cs.seek(target),
        };
        if !ok {
            return -1;
        }
        s.off = target;
        // This may be libavformat healing a failed read. A source validated at the requested byte
        // has recovered; a later callback failure will arm the bit again.
        s.io_failed = false;
        target
    }
}

/// Settle one `av_read_frame` result against errors observed by its AVIO callbacks.
fn frame_read_failed(state: &mut AvioState, result: c_int) -> bool {
    if result >= 0 {
        state.io_failed = false;
        return false;
    }
    if state.io_failed {
        crate::player::log(&format!(
            "ff: media transport failed during av_read_frame r={result}"
        ));
        SHARED.demux_io_failed.store(true, Ordering::Release);
    }
    true
}

#[inline]
unsafe fn pts_ns(pkt: *const AVPacket, st: *mut AVStream) -> i64 {
    let t = if (*pkt).pts != AV_NOPTS_VALUE {
        (*pkt).pts
    } else {
        (*pkt).dts
    };
    if t == AV_NOPTS_VALUE {
        return 0;
    }
    av_rescale_q(t, stream_time_base(st), NS_TB)
}

#[inline]
unsafe fn pts_ns_opt(pkt: *const AVPacket, st: *mut AVStream) -> Option<i64> {
    let t = if (*pkt).pts != AV_NOPTS_VALUE {
        (*pkt).pts
    } else {
        (*pkt).dts
    };
    (t != AV_NOPTS_VALUE).then(|| av_rescale_q(t, stream_time_base(st), NS_TB))
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
        96000 => 0,
        88200 => 1,
        64000 => 2,
        48000 => 3,
        44100 => 4,
        32000 => 5,
        24000 => 6,
        22050 => 7,
        16000 => 8,
        12000 => 9,
        11025 => 10,
        8000 => 11,
        7350 => 12,
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
        let ch = (*acp).ch_layout.nb_channels;
        if (1..=7).contains(&ch) {
            ch as u8
        } else {
            2
        }
    };
    // AudioSpecificConfig: aot(5) freqIdx(4) [+24-bit rate if idx==15] chanCfg(4) …
    let (ed, n) = ((*acp).extradata, (*acp).extradata_size);
    if !ed.is_null() && n >= 2 {
        let (b0, b1) = (*ed as u32, *ed.add(1) as u32);
        let aot = (b0 >> 3) & 0x1F;
        let freq_idx = (((b0 & 0x07) << 1) | (b1 >> 7)) as u8;
        if aot != 31 && freq_idx <= 12 {
            let chan = ((b1 >> 3) & 0x0F) as u8;
            let ch = if (1..=7).contains(&chan) {
                chan
            } else {
                chan_fallback
            };
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
        0xFC,                                     // fullness lo 6 (111111) | num_blocks(00)
    ]
}

#[derive(Default)]
struct H264ParamSets {
    sps: Vec<u8>,
    pps: Vec<u8>,
}

fn annexb_start(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= data.len() {
        if data[i..].starts_with(&[0, 0, 1]) {
            return Some((i, 3));
        }
        if i + 4 <= data.len() && data[i..].starts_with(&[0, 0, 0, 1]) {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

/// Validate an MPEG-TS H.264 packet as Annex-B, retain its in-band SPS/PPS and prepend whichever
/// set is missing on an IDR. The progressive demuxer cannot be reused here: it interprets the
/// first `00 00 00 01` as an AVCC length of one and corrupts the access unit.
fn ts_h264_access_unit(
    data: &[u8],
    params: &mut H264ParamSets,
    out: &mut Vec<u8>,
) -> Result<bool, &'static str> {
    out.clear();
    let Some((first, _)) = annexb_start(data, 0) else {
        return Err("H.264 packet is not Annex-B");
    };
    if first != 0 {
        return Err("bytes precede the first Annex-B start code");
    }

    let mut cursor = 0;
    let mut is_key = false;
    let mut has_sps = false;
    let mut has_pps = false;
    while let Some((start, prefix)) = annexb_start(data, cursor) {
        let payload = start + prefix;
        if payload >= data.len() {
            return Err("empty Annex-B NAL");
        }
        let end = annexb_start(data, payload).map_or(data.len(), |(next, _)| next);
        let nal_type = data[payload] & 0x1f;
        match nal_type {
            5 => is_key = true,
            7 => {
                params.sps.clear();
                params.sps.extend_from_slice(&[0, 0, 0, 1]);
                params.sps.extend_from_slice(&data[payload..end]);
                has_sps = true;
            }
            8 => {
                params.pps.clear();
                params.pps.extend_from_slice(&[0, 0, 0, 1]);
                params.pps.extend_from_slice(&data[payload..end]);
                has_pps = true;
            }
            _ => {}
        }
        cursor = end;
        if cursor >= data.len() {
            break;
        }
    }

    if is_key {
        if !has_sps {
            if params.sps.is_empty() {
                return Err("IDR has no in-band or cached SPS");
            }
            out.extend_from_slice(&params.sps);
        }
        if !has_pps {
            if params.pps.is_empty() {
                return Err("IDR has no in-band or cached PPS");
            }
            out.extend_from_slice(&params.pps);
        }
    }
    out.extend_from_slice(data);
    Ok(is_key)
}

fn packet_has_adts(data: &[u8]) -> bool {
    data.len() >= 7 && data[0] == 0xff && data[1] & 0xf6 == 0xf0
}

fn adts_duration_ns(data: &[u8]) -> Option<i64> {
    if !packet_has_adts(data) {
        return None;
    }
    const RATES: [i64; 13] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000, 7_350,
    ];
    let rate = *RATES.get(((data[2] >> 2) & 0x0f) as usize)?;
    let blocks = i64::from((data[6] & 0x03) + 1);
    Some(
        1_024_i64
            .saturating_mul(blocks)
            .saturating_mul(1_000_000_000)
            / rate,
    )
}

#[derive(Clone, Copy, Debug)]
enum HlsExit {
    Aborted,
    NotReady,
    PrimeExpired,
    /// The terminal rule fired on the ACTIVE stream: the playable reserve reached zero while this
    /// sized response still had bytes outstanding and a cheaper rung remained. Not a failure —
    /// the segment is abandoned on purpose after the reserve actually reaches its terminal
    /// boundary, so the controller gets a recovery decision it would otherwise never be asked
    /// for. See [`StallGuard`].
    ///
    /// It carries the transfer for diagnostics and censored-event accounting. Its prefix is not a
    /// capacity or acquisition observation: PMS production, response pacing and network service
    /// are not identifiable separately before the response completes.
    StallAbort(SegmentTransfer),
    Failed(&'static str),
}

fn classify_plaintext_open_failure(error: crate::stream::HttpOpenError) -> HlsExit {
    match error {
        crate::stream::HttpOpenError::Deadline => HlsExit::PrimeExpired,
        crate::stream::HttpOpenError::Status(404) => HlsExit::NotReady,
        crate::stream::HttpOpenError::Aborted => HlsExit::Aborted,
        crate::stream::HttpOpenError::Status(_) | crate::stream::HttpOpenError::Transport => {
            HlsExit::Failed("HTTP request failed")
        }
    }
}

fn hls_open_source(
    resource: &crate::hls::Resource,
    request_path: &str,
    aq: *mut AuQueue,
    hs: *mut HttpStream,
    deadline: Option<std::time::Instant>,
) -> Result<(Src, i64), HlsExit> {
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        return Err(HlsExit::Aborted);
    }
    let origin = &resource.origin;
    if origin.is_tls() {
        let reservation = crate::curlio::CurlSource::reserve_open().map_err(|e| {
            if e == crate::curlio::OpenErr::Aborted {
                HlsExit::Aborted
            } else {
                HlsExit::Failed("HTTPS reservation failed")
            }
        })?;
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            return Err(HlsExit::Aborted);
        }
        let url = format!("{}{}", origin.base(), request_path);
        let opened = match deadline {
            Some(at) => crate::curlio::CurlSource::open_reserved_until(&url, 0, reservation, at),
            None => crate::curlio::CurlSource::open_reserved(&url, 0, reservation),
        };
        // AQ abort is teardown's cross-transport linearization point. curl has its own wake latch,
        // but teardown sets AQ first; therefore an open result racing that interval must be
        // classified from AQ before a curl deadline/status can become ABR evidence.
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            drop(opened);
            return Err(HlsExit::Aborted);
        }
        let cs = opened.map_err(|e| {
            if e == crate::curlio::OpenErr::Deadline {
                return HlsExit::PrimeExpired;
            }
            if let crate::curlio::OpenErr::Status(status) = e {
                SHARED.dg_http_status.store(status, Ordering::Relaxed);
                if status == 404 {
                    return HlsExit::NotReady;
                }
            }
            if e == crate::curlio::OpenErr::Aborted {
                HlsExit::Aborted
            } else {
                HlsExit::Failed("HTTPS request failed")
            }
        })?;
        let (status, size) = (cs.status(), cs.size());
        SHARED.dg_http_status.store(status, Ordering::Relaxed);
        SHARED.file_size.store(size, Ordering::Release);
        Ok((Src::Curl(cs), size))
    } else {
        let host = CString::new(origin.host()).map_err(|_| HlsExit::Failed("invalid PMS host"))?;
        let path =
            CString::new(request_path).map_err(|_| HlsExit::Failed("invalid HLS request path"))?;
        crate::stream::http_close(hs);
        let opened = if let Some(at) = deadline {
            if std::time::Instant::now() >= at {
                return Err(HlsExit::PrimeExpired);
            }
            crate::stream::http_open_until_result(
                hs,
                host.as_ptr(),
                origin.port() as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
                at,
            )
        } else {
            let result = crate::stream::http_open(
                hs,
                host.as_ptr(),
                origin.port() as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
            );
            if result == 0 {
                Ok(())
            } else {
                let status = crate::stream::hs_status(hs);
                Err(if status > 0 {
                    crate::stream::HttpOpenError::Status(status)
                } else {
                    crate::stream::HttpOpenError::Transport
                })
            }
        };
        // Symmetric with curl above: AQ is teardown's cross-transport winner, and must be
        // observed before a typed transport result becomes playback/ABR evidence.
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            return Err(HlsExit::Aborted);
        }
        if let Err(error) = opened {
            SHARED
                .dg_http_status
                .store(crate::stream::hs_status(hs), Ordering::Relaxed);
            return Err(classify_plaintext_open_failure(error));
        }
        let status = crate::stream::hs_status(hs);
        let size = crate::stream::hs_content_length(hs);
        SHARED.dg_http_status.store(status, Ordering::Relaxed);
        SHARED.file_size.store(size, Ordering::Release);
        Ok((
            Src::Socket {
                hs,
                host,
                port: origin.port() as c_int,
                path,
            },
            size,
        ))
    }
}

fn hls_source_read(
    src: &mut Src,
    aq: *mut AuQueue,
    dst: &mut [u8],
    deadline: Option<std::time::Instant>,
) -> Result<usize, HlsExit> {
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        if let Src::Curl(cs) = src {
            cs.abort();
        }
        return Err(HlsExit::Aborted);
    }
    let read = match src {
        Src::Socket { hs, .. } => {
            crate::stream::http_read_until(*hs, dst.as_mut_ptr(), dst.len() as c_int, deadline)
        }
        Src::Curl(cs) => cs.read_until(dst, deadline),
    };
    // The wake used to interrupt a blocked body read is deliberately transport-shaped (EOF for
    // shutdown(2), negative for curl). Re-read the lane flag before interpreting that shape so a
    // user teardown can never become failed/censored playback evidence.
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        if let Src::Curl(cs) = src {
            cs.abort();
        }
        return Err(HlsExit::Aborted);
    }
    if read == crate::stream::HTTP_READ_DEADLINE || read == crate::curlio::READ_DEADLINE {
        Err(HlsExit::PrimeExpired)
    } else if read < 0 {
        Err(HlsExit::Failed("HLS response body failed"))
    } else {
        // The progressive AVIO path increments this counter in `read_cb`; segmented HLS bypasses
        // that callback and used to leave Stats for Nerds' Network Activity lane flat at zero
        // while visibly downloading playlists and segments. Count at the shared source seam so
        // socket and curl HLS mean the same thing: bytes actually handed to the demux fetcher.
        if read > 0 {
            SHARED.dg_net_rx.fetch_add(read as i64, Ordering::Relaxed);
        }
        Ok(read as usize)
    }
}

fn hls_fetch_text(
    resource: &crate::hls::Resource,
    auth: &crate::hls::InheritedAuth,
    aq: *mut AuQueue,
    hs: *mut HttpStream,
    mut reserve: Option<&mut ReserveDeadlineState>,
) -> Result<(String, u128), HlsExit> {
    const MAX_PLAYLIST_BYTES: usize = 1024 * 1024;
    let request_path = auth
        .request_path(resource)
        .map_err(|_| HlsExit::Failed("playlist credential rejected"))?;
    let started = std::time::Instant::now();
    let mut watchdog = TransportWatchdog::for_origin(&resource.origin);
    let (mut src, size) = loop {
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            return Err(HlsExit::Aborted);
        }
        if reserve
            .as_deref_mut()
            .is_some_and(|reserve| reserve.expire_if_due(false))
        {
            return Err(HlsExit::PrimeExpired);
        }
        let reserve_snapshot = reserve
            .as_deref_mut()
            .and_then(|reserve| reserve.active(false));
        let effective = watchdog.effective(reserve_snapshot);
        match hls_open_source(resource, &request_path, aq, hs, Some(effective.at)) {
            Ok(opened) => {
                // A complete response head is transport progress and begins a fresh inactivity
                // epoch for the body. Neither a stale reserve wake nor a reconnect attempt does.
                watchdog.reset();
                break opened;
            }
            Err(HlsExit::PrimeExpired) => {
                let wake =
                    observe_hls_deadline(reserve.as_deref_mut(), effective, &watchdog, false);
                if let Some(error) = classify_hls_deadline(wake) {
                    return Err(error);
                }
                // The wall projection fired while its playhead clock remained unspent. GET is
                // idempotent, so retry it with the same liveness epoch and a fresh projection.
            }
            Err(error) => return Err(error),
        }
    };
    if size > MAX_PLAYLIST_BYTES as i64 {
        return Err(HlsExit::Failed("playlist exceeds size cap"));
    }
    let mut body = Vec::with_capacity(if size > 0 { size as usize } else { 4096 });
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            return Err(HlsExit::Aborted);
        }
        if reserve
            .as_deref_mut()
            .is_some_and(|reserve| reserve.expire_if_due(false))
        {
            return Err(HlsExit::PrimeExpired);
        }
        let reserve_snapshot = reserve
            .as_deref_mut()
            .and_then(|reserve| reserve.active(false));
        let effective = watchdog.effective(reserve_snapshot);
        let n = match hls_source_read(&mut src, aq, &mut chunk, Some(effective.at)) {
            Ok(n) => n,
            Err(HlsExit::PrimeExpired) => {
                let wake =
                    observe_hls_deadline(reserve.as_deref_mut(), effective, &watchdog, false);
                if let Some(error) = classify_hls_deadline(wake) {
                    return Err(error);
                }
                // The source stays open across a body wake. Retrying here avoids both a new GET
                // and a renewed liveness lease.
                continue;
            }
            Err(error) => return Err(error),
        };
        if n == 0 {
            break;
        }
        watchdog.reset();
        if body.len().saturating_add(n) > MAX_PLAYLIST_BYTES {
            return Err(HlsExit::Failed("playlist exceeds size cap"));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    if !hls_body_complete(size, body.len() as i64) {
        return Err(HlsExit::Failed("playlist body ended before Content-Length"));
    }
    let elapsed = started.elapsed().as_millis();
    String::from_utf8(body)
        .map(|text| (text, elapsed))
        .map_err(|_| HlsExit::Failed("playlist is not UTF-8"))
}

fn hls_wait(
    aq: *mut AuQueue,
    duration: std::time::Duration,
    absolute_deadline: Option<std::time::Instant>,
) -> Result<(), HlsExit> {
    let nominal_end = std::time::Instant::now() + duration;
    let wait_until = absolute_deadline.map_or(nominal_end, |at| at.min(nominal_end));
    while std::time::Instant::now() < wait_until {
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            return Err(HlsExit::Aborted);
        }
        let left = wait_until.saturating_duration_since(std::time::Instant::now());
        std::thread::sleep(left.min(std::time::Duration::from_millis(50)));
    }
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        Err(HlsExit::Aborted)
    } else if absolute_deadline.is_some_and(|at| std::time::Instant::now() >= at) {
        Err(HlsExit::PrimeExpired)
    } else {
        Ok(())
    }
}

/// One MPEG-TS segment's entirely private FFmpeg state. PMS HLS segments are independent decode
/// units; retaining an AVFormatContext across them teaches libavformat a byte stream that does not
/// exist and makes timestamp resets/container EOF ambiguous. The custom AVIO remains transport-
/// owned exactly as in the progressive path, but every field here is retired at the segment
/// boundary before the next request starts.
struct HlsInput {
    state: Box<AvioState>,
    avio: *mut AVIOContext,
    fmt: *mut AVFormatContext,
    pkt: *mut AVPacket,
}

impl Drop for HlsInput {
    fn drop(&mut self) {
        unsafe {
            if !self.pkt.is_null() {
                av_packet_free(&mut self.pkt);
            }
            if !self.fmt.is_null() {
                avformat_close_input(&mut self.fmt);
            }
            free_avio(self.avio);
        }
        // `state` deliberately drops last: AVIO's opaque pointer names it until `free_avio`.
        let _ = &self.state;
    }
}

/// **What the HLS demuxer actually found, logged when it CHANGES.** The progressive path has said
/// this since it was written (`ff: v=#0 codec=… WxH`); the adaptive path never has, so on every
/// HLS playback — both tiers — nothing in the log stated the codec or the raster of the stream
/// being decoded. `abr: committed … 1280x720` is the CATALOG raster of the rung that was asked
/// for, which is a different claim: it is what we requested, not what arrived.
/// `docs/adaptive-playback-plan.md` §7.B names that gap.
///
/// Emitted on change rather than per segment, because a segment open happens every two seconds
/// and the interesting event is the raster moving. The line's SHAPE is deliberately identical to
/// the progressive one so `tests/run.py`'s `RE_CODEC` reads both without a second regex — which
/// is also why the stream index is written `#0` rather than the discovered one: it is the video
/// stream, and every consumer of that field treats it that way.
static HLS_LAST_VIDEO: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);

unsafe fn log_hls_video_change(fmt: *mut AVFormatContext) {
    let streams = (*fmt).streams;
    for i in 0..(*fmt).nb_streams {
        let cp = stream_codecpar(*streams.add(i as usize));
        if (*cp).codec_type != AVMEDIA_TYPE_VIDEO {
            continue;
        }
        let (id, w, h) = ((*cp).codec_id, (*cp).width, (*cp).height);
        // Pack (codec_id, width, height) into one word so the compare-and-store is a single
        // atomic and two threads cannot interleave a half-updated triple.
        let key =
            ((id as u64 & 0xffff) << 48) | ((w as u64 & 0xffff_ffff) << 16) | (h as u64 & 0xffff);
        if HLS_LAST_VIDEO.swap(key, Ordering::Relaxed) == key {
            return;
        }
        let cname = std::ffi::CStr::from_ptr(avcodec_get_name(id)).to_string_lossy();
        crate::player::log(&format!(
            "ff: v=#0 codec={cname} codec_id={id} {w}x{h} trc={} pri={} spc={} a=#1 dur_ns={}",
            (*cp).color_trc,
            (*cp).color_primaries,
            (*cp).color_space,
            SHARED.duration_ns.load(Ordering::Relaxed)
        ));
        return;
    }
}

unsafe fn hls_input(
    src: Src,
    size: i64,
    aq: *mut AuQueue,
    reserve_deadline: ReserveDeadlineState,
    transport_watchdog: TransportWatchdog,
    stall: Option<StallGuard>,
    request_started: std::time::Instant,
) -> Result<HlsInput, HlsExit> {
    let mut input = HlsInput {
        state: Box::new(AvioState {
            src,
            aq,
            off: 0,
            size,
            io_failed: false,
            body_active_us: 0,
            body_bytes: 0,
            first_byte_at: None,
            reserve_deadline,
            transport_watchdog: Some(transport_watchdog),
            stall,
            stall_aborted: false,
        }),
        avio: std::ptr::null_mut(),
        fmt: std::ptr::null_mut(),
        pkt: std::ptr::null_mut(),
    };
    let buf = av_malloc(64 * 1024) as *mut u8;
    if buf.is_null() {
        return Err(HlsExit::Failed("segment AVIO buffer allocation failed"));
    }
    input.avio = avio_alloc_context(
        buf,
        64 * 1024,
        0,
        &mut *input.state as *mut AvioState as *mut c_void,
        Some(read_cb),
        None,
        // A segment is a forward-only object. Letting libavformat seek would generate hidden
        // Range requests and could accidentally splice bytes from a different encoder object.
        None,
    );
    if input.avio.is_null() {
        free_ptr(buf as *mut c_void);
        return Err(HlsExit::Failed("segment AVIO allocation failed"));
    }
    input.fmt = avformat_alloc_context();
    if input.fmt.is_null() {
        return Err(HlsExit::Failed("segment format allocation failed"));
    }
    (*input.fmt).pb = input.avio;
    input.state.io_failed = false;
    let opened = avformat_open_input(
        &mut input.fmt,
        std::ptr::null(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    if crate::aq::aq_is_aborted(aq) {
        return Err(HlsExit::Aborted);
    }
    if let Some(exit) = classify_hls_avio_operation(
        &input.state,
        opened < 0 || input.fmt.is_null(),
        request_started,
        SHARED.hls_audio_expected.load(Ordering::Acquire) && FEED_AUDIO.load(Ordering::Relaxed),
    ) {
        return Err(exit);
    }
    if opened < 0 || input.fmt.is_null() {
        return Err(HlsExit::Failed("segment open/probe failed"));
    }
    // A successful enclosing FFmpeg operation healed any callback-level read retry.
    input.state.io_failed = false;
    let discovered = avformat_find_stream_info(input.fmt, std::ptr::null_mut());
    if crate::aq::aq_is_aborted(aq) {
        return Err(HlsExit::Aborted);
    }
    if let Some(exit) = classify_hls_avio_operation(
        &input.state,
        discovered < 0,
        request_started,
        SHARED.hls_audio_expected.load(Ordering::Acquire) && FEED_AUDIO.load(Ordering::Relaxed),
    ) {
        return Err(exit);
    }
    if discovered < 0 {
        return Err(HlsExit::Failed("segment stream discovery failed"));
    }
    input.state.io_failed = false;
    log_hls_video_change(input.fmt);
    input.pkt = av_packet_alloc();
    if input.pkt.is_null() {
        return Err(HlsExit::Failed("segment packet allocation failed"));
    }
    Ok(input)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SegmentTransfer {
    bytes: u64,
    active_us: u64,
    total_us: u64,
    audio_expected: bool,
}

struct HlsAu {
    data: Vec<u8>,
    pts_ns: i64,
    key: c_int,
    es: c_int,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AudioStamp {
    au: usize,
    raw_ns: Option<i64>,
    duration_ns: Option<i64>,
}

/// MPEG-TS may put a PES timestamp on the second AAC access unit while the first unit carries
/// only its frame duration. Resolve those holes inside one complete segment before mapping the
/// audio lane onto the normalized content timeline. We never borrow video PTS or wall time.
fn resolve_audio_stamps(stamps: &mut [AudioStamp]) -> Result<usize, &'static str> {
    let missing = stamps.iter().filter(|stamp| stamp.raw_ns.is_none()).count();
    if stamps.is_empty() || missing == 0 {
        return Ok(missing);
    }

    // A timestamped predecessor plus its own duration determines this packet's start.
    for index in 1..stamps.len() {
        if stamps[index].raw_ns.is_none() {
            if let (Some(previous), Some(duration)) =
                (stamps[index - 1].raw_ns, stamps[index - 1].duration_ns)
            {
                stamps[index].raw_ns = previous.checked_add(duration);
            }
        }
    }
    // A timestamped successor minus this packet's duration determines a leading hole.
    for index in (0..stamps.len().saturating_sub(1)).rev() {
        if stamps[index].raw_ns.is_none() {
            if let (Some(next), Some(duration)) =
                (stamps[index + 1].raw_ns, stamps[index].duration_ns)
            {
                stamps[index].raw_ns = next.checked_sub(duration);
            }
        }
    }
    // A segment with no PES timestamp at all still has an exact AAC frame clock. Anchor its
    // first frame locally at zero; SegmentClock supplies the content-time base.
    if stamps[0].raw_ns.is_none() && stamps.iter().all(|stamp| stamp.raw_ns.is_none()) {
        stamps[0].raw_ns = Some(0);
        for index in 1..stamps.len() {
            if let (Some(previous), Some(duration)) =
                (stamps[index - 1].raw_ns, stamps[index - 1].duration_ns)
            {
                stamps[index].raw_ns = previous.checked_add(duration);
            }
        }
    }
    if stamps.iter().any(|stamp| stamp.raw_ns.is_none()) {
        return Err("AAC timestamp hole has no duration anchor");
    }
    if stamps
        .windows(2)
        .any(|pair| pair[1].raw_ns.expect("resolved") < pair[0].raw_ns.expect("resolved"))
    {
        return Err("AAC timestamps move backwards");
    }
    Ok(missing)
}

struct HlsSegmentOutput {
    aus: Vec<HlsAu>,
    transfer: SegmentTransfer,
    video_width: i32,
    video_height: i32,
    video_tail_ns: i64,
    audio_tail_ns: Option<i64>,
}

/// A body with a declared length is complete only after every declared byte reached AVIO.  FFmpeg
/// finding some packets is not a substitute: a socket timeout used to look like EOF, after which
/// the caller credited the segment's full EXTINF and manufactured playable reserve that was never
/// downloaded.
fn hls_body_complete(declared_size: i64, delivered: i64) -> bool {
    declared_size < 0 || delivered >= declared_size
}

unsafe fn hls_demux_segment(
    segment: &crate::hls::Segment,
    auth: &crate::hls::InheritedAuth,
    clock: &mut crate::hls::SegmentClock,
    aq: *mut AuQueue,
    hs: *mut HttpStream,
    acodec: &str,
    mut reserve_deadline: ReserveDeadlineState,
    stall: Option<StallGuard>,
) -> Result<HlsSegmentOutput, HlsExit> {
    if crate::aq::aq_is_aborted(aq) {
        return Err(HlsExit::Aborted);
    }
    let path = auth
        .request_path(&segment.resource)
        .map_err(|_| HlsExit::Failed("segment credential rejected"))?;
    let request_started = std::time::Instant::now();
    let retry_budget = segment
        .duration
        .saturating_mul(3)
        .saturating_add(std::time::Duration::from_secs(2))
        .clamp(
            std::time::Duration::from_secs(3),
            std::time::Duration::from_secs(15),
        );
    let mut not_ready_retries = 0u32;
    let mut transport_watchdog = TransportWatchdog::for_origin(&segment.resource.origin);
    // The SUCCESSFUL attempt only. A `NotReady` retry is the server saying the segment does not
    // exist yet, which is production latency of a different kind and is already counted by
    // `not_ready=`; folding it in here would make one number mean two things.
    let open_us;
    let (src, size) = loop {
        if crate::aq::aq_is_aborted(aq) {
            return Err(HlsExit::Aborted);
        }
        if reserve_deadline.expire_if_due(SHARED.hls_rebuffering.load(Ordering::Acquire)) {
            return Err(HlsExit::PrimeExpired);
        }
        let attempt = std::time::Instant::now();
        let rebuffering = SHARED.hls_rebuffering.load(Ordering::Acquire);
        let reserve_snapshot = reserve_deadline.active(rebuffering);
        let effective = transport_watchdog.effective(reserve_snapshot);
        match hls_open_source(&segment.resource, &path, aq, hs, Some(effective.at)) {
            Ok(opened) => {
                transport_watchdog.reset();
                open_us = attempt.elapsed().as_micros() as u64;
                break opened;
            }
            // The blocking open may have returned on the old reserve deadline in the same pump
            // tick that established the internal hold. Re-open under the promoted policy; the
            // ordinary transport liveness bound remains, only the rollback deadline disappears.
            Err(HlsExit::PrimeExpired) => {
                let current_rebuffering = SHARED.hls_rebuffering.load(Ordering::Acquire);
                let wake = observe_hls_deadline(
                    Some(&mut reserve_deadline),
                    effective,
                    &transport_watchdog,
                    current_rebuffering,
                );
                if let Some(error) = classify_hls_deadline(wake) {
                    return Err(error);
                }
                continue;
            }
            // **The retry is INSIDE the caller's deadline, and it was not.** `retry_budget` runs
            // to 15 s and was the only bound here, while `deadline` reached only as far as
            // `hls_input` — which is constructed after this loop exits. So a candidate whose
            // deadline was three seconds could spend fifteen looping on `NotReady` before the
            // deadline had any effect at all, which is R19's "the `NotReady` retry has no leg of
            // its own" seen from the enforcement side rather than the accounting side. A retry
            // budget that can outlive the reserve it is spending is not a budget.
            Err(HlsExit::NotReady) if request_started.elapsed() < retry_budget => {
                not_ready_retries = not_ready_retries.saturating_add(1);
                transport_watchdog.reset();
                if crate::aq::aq_is_aborted(aq) {
                    return Err(HlsExit::Aborted);
                }
                if reserve_deadline.expire_if_due(SHARED.hls_rebuffering.load(Ordering::Acquire)) {
                    return Err(HlsExit::PrimeExpired);
                }
                // Never sleep past the deadline. A fixed 250 ms wait against a deadline 40 ms away
                // overshoots by 210 ms of reserve, every time, for no information — the poll after
                // it can only be discarded.
                let mut wait = std::time::Duration::from_millis(250);
                let rebuffering = SHARED.hls_rebuffering.load(Ordering::Acquire);
                let reserve_snapshot = reserve_deadline.active(rebuffering);
                let effective = transport_watchdog.effective(reserve_snapshot);
                wait = wait.min(
                    effective
                        .at
                        .saturating_duration_since(std::time::Instant::now()),
                );
                match hls_wait(aq, wait, Some(effective.at)) {
                    Err(HlsExit::PrimeExpired) => {
                        let current_rebuffering = SHARED.hls_rebuffering.load(Ordering::Acquire);
                        let wake = observe_hls_deadline(
                            Some(&mut reserve_deadline),
                            effective,
                            &transport_watchdog,
                            current_rebuffering,
                        );
                        if let Some(error) = classify_hls_deadline(wake) {
                            return Err(error);
                        }
                    }
                    result => result?,
                }
            }
            Err(HlsExit::NotReady) => {
                return Err(HlsExit::Failed("HLS segment was not produced in time"))
            }
            Err(error) => return Err(error),
        }
    };
    let body_started = std::time::Instant::now();
    let mut input = hls_input(
        src,
        size,
        aq,
        reserve_deadline,
        transport_watchdog,
        stall,
        request_started,
    )?;
    let probe_done = std::time::Instant::now();
    let streams = (*input.fmt).streams;
    let vi = av_find_best_stream(
        input.fmt,
        AVMEDIA_TYPE_VIDEO,
        -1,
        -1,
        std::ptr::null_mut(),
        0,
    );
    let ai = audio_stream_matching(input.fmt, acodec).unwrap_or_else(|| {
        av_find_best_stream(
            input.fmt,
            AVMEDIA_TYPE_AUDIO,
            -1,
            -1,
            std::ptr::null_mut(),
            0,
        )
    });
    if vi < 0 {
        return Err(HlsExit::Failed("HLS segment has no video stream"));
    }
    let vst = *streams.add(vi as usize);
    let vcp = stream_codecpar(vst);
    if (*vcp).codec_id != AV_CODEC_ID_H264 {
        return Err(HlsExit::Failed("HLS segment video is not H.264"));
    }
    let aac_adts = if ai >= 0 {
        let acp = stream_codecpar(*streams.add(ai as usize));
        if (*acp).codec_id != AV_CODEC_ID_AAC {
            return Err(HlsExit::Failed("HLS segment audio is not AAC"));
        }
        adts_params(acp)
    } else {
        None
    };

    let video_width = (*vcp).width;
    let video_height = (*vcp).height;
    if video_width <= 0 || video_height <= 0 {
        return Err(HlsExit::Failed("HLS segment has invalid video dimensions"));
    }
    let mut params = H264ParamSets::default();
    let mut aubuf = Vec::with_capacity(4 * 1024 * 1024);
    let mut video_packets = 0usize;
    let mut audio_packets = 0usize;
    let mut audio_stamps = Vec::new();
    let mut first_au_at = None;
    let mut aus = Vec::new();
    let mut video_tail_ns = -1;
    let mut audio_tail_ns = None;

    loop {
        input.state.io_failed = false;
        let read = av_read_frame(input.fmt, input.pkt);
        if crate::aq::aq_is_aborted(aq) {
            return Err(HlsExit::Aborted);
        }
        if let Some(exit) = classify_hls_avio_operation(
            &input.state,
            read < 0,
            request_started,
            ai >= 0 && FEED_AUDIO.load(Ordering::Relaxed),
        ) {
            return Err(exit);
        }
        if read < 0 {
            break;
        }
        let si = (*input.pkt).stream_index;
        if si == vi {
            let Some(raw_pts) = pts_ns_opt(input.pkt, vst) else {
                av_packet_unref(input.pkt);
                return Err(HlsExit::Failed("HLS video packet has no timestamp"));
            };
            let is_key = ts_h264_access_unit(
                std::slice::from_raw_parts((*input.pkt).data, (*input.pkt).size.max(0) as usize),
                &mut params,
                &mut aubuf,
            )
            .map_err(HlsExit::Failed)?;
            if video_packets == 0 && !is_key {
                av_packet_unref(input.pkt);
                return Err(HlsExit::Failed("HLS segment does not begin with an IDR"));
            }
            let pts = clock.normalize_video(raw_pts).0.saturating_mul(1_000_000);
            aus.push(HlsAu {
                data: aubuf.clone(),
                pts_ns: pts,
                key: if is_key { 1 } else { 0 },
                es: 1,
            });
            video_tail_ns = video_tail_ns.max(pts);
            video_packets += 1;
            first_au_at.get_or_insert_with(std::time::Instant::now);
            av_packet_unref(input.pkt);
        } else if si == ai && FEED_AUDIO.load(Ordering::Relaxed) {
            let ast = *streams.add(ai as usize);
            let raw_pts = pts_ns_opt(input.pkt, ast);
            let packet_duration = ((*input.pkt).duration > 0)
                .then(|| av_rescale_q((*input.pkt).duration, stream_time_base(ast), NS_TB))
                .filter(|duration| *duration > 0);
            let aac_duration = aac_adts.and_then(|(freq_idx, _)| {
                const RATES: [i64; 13] = [
                    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000,
                    11_025, 8_000, 7_350,
                ];
                RATES
                    .get(freq_idx as usize)
                    .map(|rate| 1_024_i64.saturating_mul(1_000_000_000) / rate)
            });
            let raw =
                std::slice::from_raw_parts((*input.pkt).data, (*input.pkt).size.max(0) as usize);
            let duration_ns = packet_duration
                .or_else(|| adts_duration_ns(raw))
                .or(aac_duration);
            let data = if packet_has_adts(raw) {
                raw.to_vec()
            } else if let Some((freq_idx, chan_cfg)) = aac_adts {
                let mut framed = Vec::with_capacity(7 + raw.len());
                framed.extend_from_slice(&adts_header(freq_idx, chan_cfg, raw.len()));
                framed.extend_from_slice(raw);
                framed
            } else {
                av_packet_unref(input.pkt);
                return Err(HlsExit::Failed("AAC packet is neither ADTS nor reframable"));
            };
            let au = aus.len();
            aus.push(HlsAu {
                data,
                pts_ns: 0,
                key: 1,
                es: 2,
            });
            audio_stamps.push(AudioStamp {
                au,
                raw_ns: raw_pts,
                duration_ns,
            });
            audio_packets += 1;
            av_packet_unref(input.pkt);
        } else {
            av_packet_unref(input.pkt);
        }
        if crate::aq::aq_is_aborted(aq) {
            return Err(HlsExit::Aborted);
        }
    }
    if crate::aq::aq_is_aborted(aq) {
        return Err(HlsExit::Aborted);
    }
    if input
        .state
        .reserve_deadline
        .expire_if_due(SHARED.hls_rebuffering.load(Ordering::Acquire))
    {
        return Err(HlsExit::PrimeExpired);
    }

    if !hls_body_complete(input.state.size, input.state.off) {
        crate::player::log(&format!(
            "hls: short-body segment={} got={} want={}",
            segment.sequence, input.state.off, input.state.size,
        ));
        return Err(HlsExit::Failed("segment body ended before Content-Length"));
    }

    if video_packets == 0 {
        return Err(HlsExit::Failed(
            "HLS segment produced no video access units",
        ));
    }
    let synthesized_audio = resolve_audio_stamps(&mut audio_stamps).map_err(HlsExit::Failed)?;
    for stamp in &audio_stamps {
        let pts = clock
            .normalize_audio(stamp.raw_ns.expect("audio stamps resolved"))
            .0
            .saturating_mul(1_000_000);
        aus[stamp.au].pts_ns = pts;
        audio_tail_ns = Some(audio_tail_ns.map_or(pts, |tail: i64| tail.max(pts)));
    }
    // Every AU in this independently decodable segment has been read successfully. Buffer health
    // is the end of decoded content, not the start timestamp of the final video frame; using the
    // latter made a complete 2 s / 24 fps segment look 42 ms short and forced an immediate drop.
    let segment_end_ns = clock.end().0.saturating_mul(1_000_000);
    video_tail_ns = video_tail_ns.max(segment_end_ns);
    if audio_packets > 0 {
        audio_tail_ns = Some(audio_tail_ns.map_or(segment_end_ns, |tail| tail.max(segment_end_ns)));
    }
    let first_ms = first_au_at
        .map(|at| at.duration_since(request_started).as_millis())
        .unwrap_or_default();
    let transfer = SegmentTransfer {
        bytes: input.state.body_bytes,
        active_us: input.state.body_active_us,
        total_us: request_started.elapsed().as_micros().max(1) as u64,
        audio_expected: ai >= 0 && FEED_AUDIO.load(Ordering::Relaxed),
    };
    crate::player::log(&format!(
        "hls: segment={} bytes={} raster={}x{} v={} a={} tail_skew_ms={} audio_pts_recovered={} \
         not_ready={} open_ms={} ttfb_ms={} open_probe_ms={} first_au_ms={} total_ms={}",
        segment.sequence,
        transfer.bytes,
        video_width,
        video_height,
        video_packets,
        audio_packets,
        audio_tail_ns.map_or(0, |audio| (video_tail_ns - audio) / 1_000_000),
        synthesized_audio,
        not_ready_retries,
        open_us / 1_000,
        input
            .state
            .first_byte_at
            .map(|at| at.duration_since(body_started).as_millis() as i64)
            .unwrap_or(-1),
        probe_done.duration_since(body_started).as_millis(),
        first_ms,
        transfer.total_us / 1_000,
    ));
    Ok(HlsSegmentOutput {
        aus,
        transfer,
        video_width,
        video_height,
        video_tail_ns,
        audio_tail_ns,
    })
}

fn hls_feed_segment(
    output: &HlsSegmentOutput,
    aq: *mut AuQueue,
    aqa: *mut AuQueue,
) -> Result<(), HlsExit> {
    SHARED
        .hls_audio_expected
        .store(output.transfer.audio_expected, Ordering::Release);
    // Close the race with the main-thread pump: before the first AU can satisfy the decoder prime,
    // publish how long this acquisition actually cost.  The pump then requires at least that much
    // playable media.  Do NOT lower the published value at the end of this function: the AUs are
    // then in our queues, not necessarily accepted by Starfish yet, and the controller has not yet
    // replaced the previous finite-bag runway with the window that includes this acquisition.  A
    // store here opened a small but real interval in which the pump could start against only the
    // latest (cheaper) sample.  `Controller::prime_runway_ms` publishes the new complete value
    // immediately after `observe` below.
    let acquisition_ms =
        i64::try_from(output.transfer.total_us.saturating_add(999) / 1_000).unwrap_or(i64::MAX);
    SHARED
        .hls_prime_runway_ms
        .fetch_max(acquisition_ms, Ordering::AcqRel);
    // A candidate is demuxed before it owns playback. Publish one coherent raster only after its
    // first video AU is accepted by the active queue; a rejected candidate and a failed first push
    // must remain invisible to the video-plane actuator.
    let mut raster_published = false;
    for au in &output.aus {
        let queue = if au.es == 1 { aq } else { aqa };
        if crate::aq::aq_push(
            queue,
            au.data.as_ptr(),
            au.data.len() as c_int,
            au.pts_ns,
            au.key,
            au.es,
        ) != 0
        {
            return Err(HlsExit::Aborted);
        }
        if au.es == 1 {
            if !raster_published {
                SHARED.publish_video_raster(output.video_width, output.video_height);
                raster_published = true;
            }
            PUSHED_ANY.store(true, Ordering::Relaxed);
            SHARED.hls_video_tail_ns.store(au.pts_ns, Ordering::Release);
        } else {
            SHARED.hls_audio_tail_ns.store(au.pts_ns, Ordering::Release);
        }
    }
    Ok(())
}

fn hls_raster_within(width: i32, height: i32, rung: crate::abr::Rung) -> bool {
    let (max_width, max_height) = rung.raster();
    width > 0 && height > 0 && width <= i32::from(max_width) && height <= i32::from(max_height)
}

/// A larger request is a quality transition only when its OBSERVED output strictly dominates the
/// stream it would replace.  Raster and the master playlist's declared bandwidth are independent
/// axes: more bits at the same raster can improve an encode, and more pixels at a lower declared
/// rate is not orderable without inventing a subjective weight.  Pareto dominance is therefore
/// the strongest statement the measurements alone support — no resolution target, ratio or
/// tolerance is introduced.
fn hls_upshift_strictly_improves(
    current_width: i32,
    current_height: i32,
    current_declared_bps: u64,
    candidate_width: i32,
    candidate_height: i32,
    candidate_declared_bps: u64,
) -> bool {
    let Some(current) =
        crate::abr::ObservedHlsVariant::new(current_declared_bps, current_width, current_height)
    else {
        return false;
    };
    crate::abr::ObservedHlsVariant::new(candidate_declared_bps, candidate_width, candidate_height)
        .is_some_and(|candidate| candidate.strictly_dominates(current))
}

struct HlsCursor {
    /// Whether this cursor's media playlist is the one describing WHAT IS PLAYING.
    ///
    /// A candidate cursor opened during a quality transaction reads a freshly created encoder
    /// session, whose playlist lists only the segments PMS has produced so far — so its
    /// `total_duration()` is a fraction of the film's. Publishing that clobbers
    /// `SHARED.duration_ns`, and on a reject nothing ever restores it: the remaining-playback
    /// estimate, the HUD's total and the scrobble all inherit the truncated number for the rest
    /// of the playback. The candidate is promoted to publishing at the moment it is committed.
    publishes_duration: bool,
    /// **The rate this rendition's master playlist DECLARED**, bit/s, `#EXT-X-STREAM-INF:BANDWIDTH`.
    ///
    /// Retained rather than logged and dropped, which is what happened to it until now. It is the
    /// only per-rung rate this app can obtain that is not the catalog's `expected_wire_kbps`, whose
    /// error is +5.2% to +31.6%, item-dependent and non-injective (rungs 18000 and 20000 both
    /// declare 16 150). Live candidate acceptance uses measured `(A,D,B_post)`, not this declaration;
    /// keeping it attached to the cursor makes the transaction trace describe the actual PMS
    /// variant and preserves the historical corpus comparator.
    ///
    /// Available at VALIDATION and nowhere else: a rung's `BANDWIDTH` cannot be read without first
    /// creating a PMS encoder session for it, so selection over the ladder still has no per-rung
    /// rate. That asymmetry is the specification's, not this field's.
    declared_bps: u64,
    auth: crate::hls::InheritedAuth,
    media: crate::hls::Resource,
    tracker: crate::hls::MediaTracker,
    pending: std::collections::VecDeque<crate::hls::Segment>,
    ended: bool,
    target_duration_secs: u64,
    start_applied: bool,
}

fn hls_cursor_open(
    origin: &crate::plex::Origin,
    path: &str,
    aq: *mut AuQueue,
    hs: *mut HttpStream,
    publishes_duration: bool,
    reserve: Option<&mut ReserveDeadlineState>,
) -> Result<HlsCursor, HlsExit> {
    let master_resource = crate::hls::Resource::new(origin.clone(), path)
        .map_err(|_| HlsExit::Failed("invalid HLS master URL"))?;
    let auth = crate::hls::InheritedAuth::capture(&master_resource)
        .map_err(|_| HlsExit::Failed("HLS master has no unique credential"))?;
    let (master_text, master_ms) = hls_fetch_text(&master_resource, &auth, aq, hs, reserve)?;
    let master = crate::hls::parse_master(&master_resource, &master_text).map_err(|e| {
        crate::player::log(&format!("hls: master rejected: {e}"));
        HlsExit::Failed("HLS master rejected")
    })?;
    crate::player::log(&format!(
        "hls: master one-variant bandwidth={} fetch_ms={master_ms}",
        master.variant.bandwidth
    ));
    Ok(HlsCursor {
        publishes_duration,
        declared_bps: master.variant.bandwidth,
        auth,
        media: master.variant.resource,
        tracker: crate::hls::MediaTracker::default(),
        pending: std::collections::VecDeque::new(),
        ended: false,
        target_duration_secs: 1,
        start_applied: false,
    })
}

fn hls_cursor_next(
    cursor: &mut HlsCursor,
    aq: *mut AuQueue,
    hs: *mut HttpStream,
    mut reserve: Option<&mut ReserveDeadlineState>,
) -> Result<Option<crate::hls::Segment>, HlsExit> {
    loop {
        if let Some(segment) = cursor.pending.pop_front() {
            return Ok(Some(segment));
        }
        if cursor.ended {
            return Ok(None);
        }
        let (media_text, media_ms) =
            hls_fetch_text(&cursor.media, &cursor.auth, aq, hs, reserve.as_deref_mut())?;
        let media = crate::hls::parse_media(&cursor.media, &media_text).map_err(|e| {
            crate::player::log(&format!("hls: media rejected: {e}"));
            HlsExit::Failed("HLS media playlist rejected")
        })?;
        cursor.target_duration_secs = media.target_duration_secs;
        let start_index = media
            .preferred_start_index()
            .map_err(|_| HlsExit::Failed("HLS start offset is outside the supported timeline"))?;
        let total_ns = i64::try_from(
            media
                .total_duration()
                .map_err(|_| HlsExit::Failed("HLS playlist duration overflow"))?
                .as_nanos(),
        )
        .map_err(|_| HlsExit::Failed("HLS playlist duration overflow"))?;
        if cursor.publishes_duration {
            SHARED.duration_ns.store(total_ns, Ordering::Relaxed);
        }
        let mut refresh = cursor.tracker.apply(&media).map_err(|e| {
            crate::player::log(&format!("hls: refresh rejected: {e}"));
            HlsExit::Failed("HLS media refresh rejected")
        })?;
        let mut skipped = 0usize;
        if !cursor.start_applied {
            let first_sequence = media
                .segments
                .get(start_index)
                .map(|segment| segment.sequence)
                .unwrap_or_else(|| {
                    media
                        .media_sequence
                        .saturating_add(media.segments.len() as u64)
                });
            let before = refresh.new_segments.len();
            refresh
                .new_segments
                .retain(|segment| segment.sequence >= first_sequence);
            skipped = before.saturating_sub(refresh.new_segments.len());
            cursor.start_applied = true;
        }
        crate::player::log(&format!(
            "hls: refresh new={} skipped={} total_ms={} end={} fetch_ms={media_ms}",
            refresh.new_segments.len(),
            skipped,
            total_ns / 1_000_000,
            refresh.end_list
        ));
        cursor.pending.extend(refresh.new_segments);
        cursor.ended = refresh.end_list;
        if cursor.pending.is_empty() && !cursor.ended {
            let poll = std::time::Duration::from_millis(
                cursor
                    .target_duration_secs
                    .saturating_mul(500)
                    .clamp(250, 1_000),
            );
            let reserve_snapshot = reserve
                .as_deref_mut()
                .and_then(|reserve| reserve.active(false));
            match hls_wait(aq, poll, reserve_snapshot) {
                Err(HlsExit::PrimeExpired) => {
                    if reserve.as_deref_mut().map_or(true, |reserve| {
                        reserve.note_transport_deadline(reserve_snapshot, false)
                    }) {
                        return Err(HlsExit::PrimeExpired);
                    }
                }
                result => result?,
            }
        }
    }
}

/// Make the rollback cursor's next media obligation explicit without consuming it.
///
/// An ABR/source experiment runs between current-stream objects. If it fails, the response that
/// must survive is therefore the NEXT parsed EXTINF, not the duration of the object which just
/// completed. Variable-duration HLS makes those different physical facts. When the next object is
/// not already queued, perform the playlist refresh the outer loop would otherwise perform on its
/// next iteration, then put the object back at the front unchanged.
fn hls_duration_obligation_ms(duration: std::time::Duration) -> Option<u32> {
    // BufferSnapshot lives on a whole-millisecond lattice and floors the playable reserve.  A
    // media obligation must therefore round in the opposite direction: flooring both B and an
    // EXTINF such as 1999.9 ms can manufacture almost one millisecond of exploration reserve.
    // The parser's nanoseconds remain exact; this is the conservative projection onto the
    // controller's current unit, not a re-parse of the playlist decimal.
    let nanos = duration.as_nanos();
    let millis = nanos.checked_add(999_999)?.checked_div(1_000_000)?;
    u32::try_from(millis).ok()
}

fn hls_cursor_next_duration_ms(
    cursor: &mut HlsCursor,
    aq: *mut AuQueue,
    hs: *mut HttpStream,
) -> Result<Option<u32>, HlsExit> {
    if let Some(segment) = cursor.pending.front() {
        return hls_duration_obligation_ms(segment.duration)
            .map(Some)
            .ok_or(HlsExit::Failed("HLS rollback duration overflow"));
    }
    let Some(segment) = hls_cursor_next(cursor, aq, hs, None)? else {
        return Ok(None);
    };
    let duration_ms = hls_duration_obligation_ms(segment.duration)
        .ok_or(HlsExit::Failed("HLS rollback duration overflow"))?;
    cursor.pending.push_front(segment);
    Ok(Some(duration_ms))
}

fn hls_buffer_snapshot_at(
    output: Option<&HlsSegmentOutput>,
    playhead_ns: i64,
) -> crate::abr::BufferSnapshot {
    let mut video = SHARED.hls_video_tail_ns.load(Ordering::Acquire);
    let mut audio = SHARED.hls_audio_tail_ns.load(Ordering::Acquire);
    let mut audio_expected = audio >= 0;
    if let Some(candidate) = output {
        video = video.max(candidate.video_tail_ns);
        if let Some(tail) = candidate.audio_tail_ns {
            audio = audio.max(tail);
        }
        audio_expected |= candidate.transfer.audio_expected;
    }
    // Starfish consumes a zero-based feed after a resume/seek while `playpos_ns` is already on the
    // movie timeline (`disp_base + fed PTS`). Translate both demux tails by the same display base
    // before the controller compares them; raw FFmpeg PTS still never cross this boundary.
    let display_base = SHARED.disp_base.load(Ordering::Relaxed).max(0);
    crate::abr::BufferSnapshot {
        playback: crate::abr::MediaTimeMs(playhead_ns.max(0) / 1_000_000),
        video_tail: crate::abr::MediaTimeMs(video.max(0).saturating_add(display_base) / 1_000_000),
        audio_tail: (audio >= 0).then_some(crate::abr::MediaTimeMs(
            audio.max(0).saturating_add(display_base) / 1_000_000,
        )),
        audio_expected,
    }
}

fn hls_buffer_snapshot_with_playhead(
    output: Option<&HlsSegmentOutput>,
) -> (crate::abr::BufferSnapshot, i64) {
    let playhead_ns = SHARED.playpos_ns.load(Ordering::Relaxed).max(0);
    (hls_buffer_snapshot_at(output, playhead_ns), playhead_ns)
}

fn hls_buffer_snapshot(output: Option<&HlsSegmentOutput>) -> crate::abr::BufferSnapshot {
    hls_buffer_snapshot_with_playhead(output).0
}

/// Progressive Original uses absolute movie PTS, unlike offset-zero HLS, so no display-base
/// translation belongs here. Both lanes must have produced post-open/post-seek timestamps before
/// an A/V stream can claim a buffer duration; otherwise the absent lane is exactly the starvation
/// the metric is meant to notice, but not yet enough evidence to start an encoder.
fn progressive_buffered_ms(audio_expected: bool) -> Option<i64> {
    let video = SHARED.hls_video_tail_ns.load(Ordering::Acquire);
    if video < 0 {
        return None;
    }
    let audio = SHARED.hls_audio_tail_ns.load(Ordering::Acquire);
    let tail = if audio_expected {
        if audio < 0 {
            return None;
        }
        video.min(audio)
    } else {
        video
    };
    Some(
        tail.saturating_sub(SHARED.playpos_ns.load(Ordering::Relaxed).max(0))
            .max(0)
            / 1_000_000,
    )
}

fn hls_segment_sample(
    output: &HlsSegmentOutput,
    duration: std::time::Duration,
) -> Option<crate::abr::SegmentSample> {
    let duration_ms = u32::try_from(duration.as_millis()).ok()?;
    let duration_obligation_ms = hls_duration_obligation_ms(duration)?;
    crate::abr::SegmentSample::new_with_obligation(
        output.transfer.bytes,
        output.transfer.active_us,
        output.transfer.total_us,
        duration_ms,
        duration_obligation_ms,
        hls_buffer_snapshot(Some(output)),
    )
}

fn hls_abr_rate_readout(sample: crate::abr::SegmentSample) -> (i64, i64, i64) {
    // The compact panel has no `complete=` column. Publishing an abandoned prefix here would
    // present its mixed PMS-production/network rate as a normal sample even though the controller
    // correctly discarded it. Keep the forensic prefix on the `abr: sample ... complete=0` log
    // line and make the unqualified panel values explicitly unknown.
    if sample.completed() {
        (
            i64::from(sample.network_kbps()),
            i64::from(sample.media_kbps()),
            i64::from(sample.production_ratio_pm()),
        )
    } else {
        (-1, -1, -1)
    }
}

fn publish_hls_abr_sample(sample: crate::abr::SegmentSample) {
    let (net_kbps, media_kbps, ratio_pm) = hls_abr_rate_readout(sample);
    SHARED.dg_abr_net_kbps.store(net_kbps, Ordering::Relaxed);
    SHARED
        .dg_abr_media_kbps
        .store(media_kbps, Ordering::Relaxed);
    // `-1` is "not knowable this segment", which the gauge has no other way to say. It is a
    // dev read-out with one i64 slot; the decision paths take the `Option` itself.
    SHARED
        .dg_abr_buffer_ms
        .store(sample.buffer.buffered_ms().unwrap_or(-1), Ordering::Relaxed);
    SHARED.dg_abr_ratio_pm.store(ratio_pm, Ordering::Relaxed);
}

fn publish_original_probe_failure(failure: crate::route::OriginalProbeFailure) {
    use crate::route::OriginalProbeFailure as F;
    let (kind, status) = match failure {
        F::HttpStatus(status) => (crate::player::ABR_FAILURE_ORIGINAL_HTTP, status),
        F::Deadline => (crate::player::ABR_FAILURE_ORIGINAL_DEADLINE, 0),
        F::Transport => (crate::player::ABR_FAILURE_ORIGINAL_TRANSPORT, 0),
        F::NoBody => (crate::player::ABR_FAILURE_ORIGINAL_NO_BODY, 0),
        F::Other => (crate::player::ABR_FAILURE_ORIGINAL_OPEN, 0),
    };
    crate::player::note_original_failure(kind, status);
}

fn log_original_mode_comparison(cmp: crate::abr::ModeComparison) {
    let loser = cmp.loser.unwrap_or_default();
    crate::player::log(&format!(
        "abr: mode chose={:?} why={:?} vs_hls={}kbps scale={}pm \
         win[q={} f={} r={} s={} t={} tot={}] \
         lose[q={} f={} r={} s={} t={} tot={}]",
        cmp.chosen,
        cmp.reason,
        cmp.hls_rung.kbps(),
        cmp.scale_pm,
        cmp.winner.quality,
        cmp.winner.features,
        cmp.winner.risk,
        cmp.winner.server,
        cmp.winner.transition,
        cmp.winner.total,
        loser.quality,
        loser.features,
        loser.risk,
        loser.server,
        loser.transition,
        loser.total,
    ));
}

/// One line per steady-state decision, carrying everything the decision was made ON. Nothing here
/// is a name, an address, a title or a token — it is rates, milliseconds and per-mille — which is
/// what makes it safe to paste into an issue thread. `abr::Controller::telemetry` assembles it, so
/// the values logged are the values used rather than a second reading taken at the log site.
/// **The model's own state, published on EVERY segment.**
///
/// Split out of [`log_hls_abr_steady`] on 2026-08-26, and the split is the whole point. Both used
/// to be one function called only on `Decision::Stay` — so the panel's safe budget, uncertainty,
/// buffer slope, starvation horizon, risk score and reason code were refreshed **only on segments
/// where the controller decided to do nothing**. Every segment where those numbers were
/// interesting — a short horizon, end-to-end acquisition behind real time, a reserve about to run
/// out — is by
/// definition a segment that returned `Prime`, and skipped the publish.
///
/// The visible symptom was `risk 0` essentially always, which reads as "the model never sees any
/// risk" rather than "the panel is only ever shown the quiet samples". The LOG line stays on the
/// `Stay` path, because it is titled `abr: steady` and a steady line on a segment that primed a
/// candidate would be a false statement about what happened.
fn publish_hls_abr_model(t: &crate::abr::ControllerTelemetry) {
    // Bound first and stored on ONE line each, deliberately: `shared.rs`'s writer guard greps the
    // literal `dg_<field>.store(`, so a `SHARED\n    .dg_x\n    .store(` that rustfmt produces
    // reads to it exactly like a field NOTHING writes — which is the bug that guard exists to catch.
    let rel = Ordering::Relaxed;
    let optimal = t.optimal.map(|c| i64::from(c.rung.kbps())).unwrap_or(-1);
    let starve = t.risk.starvation_seconds.map(i64::from).unwrap_or(-1);
    let pred = t.risk.production_ratio_pm.map(i64::from).unwrap_or(-1);
    SHARED
        .dg_abr_safe_kbps
        .store(i64::from(t.safe_budget_kbps), rel);
    SHARED.dg_abr_optimal_kbps.store(optimal, rel);
    SHARED
        .dg_abr_unc_pm
        .store(i64::from(t.delivery.uncertainty_pm), rel);
    SHARED
        .dg_abr_samples
        .store(i64::from(t.delivery.samples), rel);
    SHARED
        .dg_abr_slope_ms_per_s
        .store(t.buffer.slope_ms_per_s, rel);
    SHARED.dg_abr_starve_secs.store(starve, rel);
    SHARED.dg_abr_pred_pm.store(pred, rel);
    SHARED.dg_abr_risk.store(i64::from(t.risk.score), rel);
    SHARED.dg_abr_why.store(abr_why_code(t.reason), rel);
    // **The seed that survives a seek** (I8). Published every segment rather than at teardown,
    // because a teardown has several paths and one of them (a crash of the demux worker) reaches
    // none of them — and the estimate is worth carrying whatever ended the session. Not a `dg_`
    // field: this coherent estimate is read to DECIDE, by `route::auto_prior` when the next
    // control is built.
    SHARED.publish_abr_seed(t.delivery);
}

/// The once-a-segment event-log line, on the do-nothing path only. Its counterpart is
/// [`publish_hls_abr_model`]; both read ONE `telemetry()` at the call site, so the line and the
/// panel can never describe two different segments.
fn log_hls_abr_steady(t: &crate::abr::ControllerTelemetry, remaining_ms: i64) {
    crate::player::log(&format!(
        "abr: steady current={}kbps safe={}kbps pending={}kbps fast={}kbps slow={}kbps unc={}pm n={} \
         buf={}ms slope={}ms/s prod={}pm/{}pm risk={} starve={} edge={} left={}s \
         dwell={}ms block={}kbps onrung={} draining={} reason={:?}",
        t.current.kbps(),
        t.safe_budget_kbps,
        t.pending.map(|p| p.rung.kbps()).unwrap_or(0),
        t.delivery.fast_kbps,
        t.delivery.slow_kbps,
        t.delivery.uncertainty_pm,
        t.delivery.samples,
        t.buffer.buffered_ms,
        t.buffer.slope_ms_per_s,
        t.production.ratio_pm,
        t.risk.production_ratio_pm.unwrap_or(0),
        t.risk.score,
        t.risk
            .starvation_seconds
            .map(|s| s.to_string())
            .unwrap_or_else(|| "none".to_string()),
        // `edge=` beside `starve=`: the same formula on the MEASURED rate rather than the
        // conservative one, and the one the emergency downshift actually reads. They differ by up
        // to 2x on the first sample of every rung, where `uncertainty_pm` is at its 500 cap, so a
        // reader grading a downshift against `starve=` alone is grading a number that decided
        // nothing.
        t.emergency_horizon_secs
            .map(|s| s.to_string())
            .unwrap_or_else(|| "none".to_string()),
        remaining_ms / 1_000,
        // `dwell=` is retained in the diagnostics wire shape and is always zero: exploration has
        // no elapsed-time release. `block=` is the highest rung the per-rung failure frontier is
        // currently refusing at this exact measured surplus.
        t.gates.dwell_ms,
        t.gates.blocked_kbps,
        t.gates.on_rung,
        t.gates.draining,
        t.reason,
    ));
}

/// **One line per SEGMENT, whatever was decided** — the decision-independent half of the trace.
///
/// `abr: steady` cannot serve this purpose and must not be made to: it is emitted only on
/// `Decision::Stay`, so the segments it omits are exactly the ones where the reserve was lowest
/// (a drawdown is what produces a downshift). A minimum-buffer statistic read from `abr: steady`
/// therefore cannot see the trough, and any statistic derived from those lines is an order
/// statistic over a sample whose membership the policy under test controls — a policy that commits
/// more often observes less, and reads as an improvement. Plan I0-A.
///
/// Every field is a rate, a duration or a per-mille. No name, address, title or token, so this is
/// safe to paste into an issue thread.
///
/// * `buf` — the controller-visible playable reserve, `min(video, audio) - playback`. **The same
///   quantity the decision path used**, taken from the same `SegmentSample`, never recomputed.
/// * `vbuf` / `abuf` — the two lanes separately, because the reserve is the MINIMUM of them and
///   which one binds moves with the rung: the 8 MiB video queue against a multi-Mbit stream
///   against a 1 MiB audio queue at ~192 kbps. `buf` alone cannot say which ceiling was hit.
/// * `media` — what the segment actually WAS on the wire, bytes over content duration. This is
///   the denominator of the reachable reserve and it is NOT `current`: eleven of the thirteen
///   catalog entries carry the request as their planning rate. Measurement step M4 reads it.
/// * `net` — delivered rate over ACTIVE transfer time; `prod` — total acquisition over content
///   duration, so it includes production and TTFB.
/// * `dur` — the segment's CONTENT duration. Two things need it and neither can infer it: the
///   harness places a segment's transfer span on a timeline as `media x dur / net`, which is how a
///   sample is attributed to an injected shaper leg without asking the controller anything; and
///   several of the controller's own guards are denominated in multiples of it
///   (`buffered >= 3 * segment`), so whether the server honoured the client's 2 s request is a
///   fact about whether those guards are reachable at all.
fn log_hls_abr_sample(
    t: &crate::abr::ControllerTelemetry,
    sample: crate::abr::SegmentSample,
    decision: crate::abr::Decision,
) {
    use crate::abr::{Decision::Prime, Direction};
    let (action, target) = match decision {
        Prime(p) if p.direction == Direction::Up && p.rung == t.current => {
            ("refresh", p.rung.kbps())
        }
        Prime(p) if p.direction == Direction::Up => ("prime_up", p.rung.kbps()),
        Prime(p) => ("prime_down", p.rung.kbps()),
        _ => ("stay", 0),
    };
    crate::player::log(&format!(
        "abr: sample current={}kbps media={}kbps net={}kbps buf={} vbuf={}ms abuf={} \
         dur={}ms prod={}pm n={} decision={} target={}kbps complete={} reason={:?}",
        t.current.kbps(),
        sample.media_kbps(),
        sample.network_kbps(),
        sample
            .buffer
            .buffered_ms()
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "none".to_string()),
        sample.buffer.video_buffered_ms(),
        sample
            .buffer
            .audio_buffered_ms()
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "none".to_string()),
        sample.media_duration_ms(),
        sample.production_ratio_pm(),
        t.delivery.samples,
        action,
        target,
        u8::from(sample.completed()),
        t.reason,
    ));
}

/// **One line per segment for exact finite-bag conservation on the current rung.**
///
/// It is the arithmetic the controller just used: the full current operating-point bag at actual
/// response sizes. Candidate validation is logged separately from the candidate's own completed
/// `(A,D,B_post)` evidence; no current response is rescaled into that candidate.
///
/// The formatting itself lives on [`crate::abr::AdmissionReadout::log_line`], beside the numbers
/// it prints and beside the test that pins the exact wire form — this function is only the
/// plumbing that decides WHEN it is emitted, which is every segment whatever was decided.
fn log_hls_abr_window(t: &crate::abr::ControllerTelemetry, sample: crate::abr::SegmentSample) {
    crate::player::log(&t.window.log_line(
        t.current.kbps(),
        sample.bytes(),
        sample.media_duration_ms(),
    ));
}

/// The controller's reason enum as the read-out's code. A `match` rather than a cast, so a new
/// [`crate::abr::HlsReason`] variant is a compile error here instead of silently reading as one of
/// the four the panel already names.
fn abr_why_code(reason: Option<crate::abr::DecisionReason>) -> u8 {
    use crate::abr::{DecisionReason::Hls, HlsReason as R};
    match reason {
        None => crate::player::ABR_WHY_NONE,
        Some(Hls(R::SafeBudgetIncrease)) => crate::player::ABR_WHY_SAFE_BUDGET,
        Some(Hls(R::UnsafeCurrentState)) => crate::player::ABR_WHY_UNSAFE_STATE,
        Some(Hls(R::ProductionConstraint)) => crate::player::ABR_WHY_PRODUCTION,
        Some(Hls(R::BufferConstraint)) => crate::player::ABR_WHY_BUFFER,
        Some(Hls(R::LadderFloor)) => crate::player::ABR_WHY_LADDER_FLOOR,
        Some(Hls(R::RejectBackoff)) => crate::player::ABR_WHY_REJECT_BACKOFF,
        Some(Hls(R::StarvationHorizon)) => crate::player::ABR_WHY_STARVATION,
        Some(Hls(R::EvidenceWindow)) => crate::player::ABR_WHY_EVIDENCE,
        Some(Hls(R::AtBestRung)) => crate::player::ABR_WHY_AT_BEST,
        Some(Hls(R::ReserveUnknown)) => crate::player::ABR_WHY_RESERVE_UNKNOWN,
        Some(Hls(R::DeadlineRollback)) => crate::player::ABR_WHY_DEADLINE_ROLLBACK,
        Some(Hls(R::ResponseLimited)) => crate::player::ABR_WHY_RESPONSE_LIMITED,
        Some(Hls(R::SwitchCost)) => crate::player::ABR_WHY_SWITCH_COST,
    }
}

/// Content still to play, from the one published duration and position. Feeds the utility model's
/// benefit scaling: a visible mode switch has to earn back its cost out of what is LEFT, which is
/// why "twenty seconds remaining" needs no special case anywhere below.
fn remaining_playback_ms() -> i64 {
    let duration = SHARED.duration_ns.load(Ordering::Relaxed);
    if duration <= 0 {
        return 0;
    }
    (duration - SHARED.playpos_ns.load(Ordering::Relaxed).max(0)).max(0) / 1_000_000
}

fn publish_hls_abr_action(
    proposal: crate::abr::Proposal,
    refreshing_same_request: bool,
    committed: Option<bool>,
) {
    use crate::abr::Direction::{Down, Up};
    let action = if refreshing_same_request {
        match committed {
            None => crate::player::ABR_ACTION_PRIME_REFRESH,
            Some(true) => crate::player::ABR_ACTION_COMMIT_REFRESH,
            Some(false) => crate::player::ABR_ACTION_REJECT_REFRESH,
        }
    } else {
        match (proposal.direction, committed) {
            (Down, None) => crate::player::ABR_ACTION_PRIME_DOWN,
            (Up, None) => crate::player::ABR_ACTION_PRIME_UP,
            (Down, Some(true)) => crate::player::ABR_ACTION_COMMIT_DOWN,
            (Up, Some(true)) => crate::player::ABR_ACTION_COMMIT_UP,
            (Down, Some(false)) => crate::player::ABR_ACTION_REJECT_DOWN,
            (Up, Some(false)) => crate::player::ABR_ACTION_REJECT_UP,
        }
    };
    SHARED.dg_abr_action.store(action, Ordering::Relaxed);
    SHARED
        .dg_abr_target_kbps
        .store(i64::from(proposal.rung.kbps()), Ordering::Relaxed);
}

/// **One record per candidate transaction, emitted on EVERY exit path.** Increment I2's
/// instrumentation; it changes no decision.
///
/// A `Drop` guard rather than a log call at each `continue`, because the prime arm has twelve
/// distinct reject paths (`control.prime` refusing, an origin change, the master playlist, the
/// media playlist, two demux legs, the timeline, acceptance, the raster check) and a thirteenth
/// added later would silently stop being measured. Drop cannot be forgotten.
///
/// **What it was introduced for.** The claim that a transaction's budget can be derived from the
/// reserve (`T = B - A_i`) rests on the transaction's real cost. Before this trace and the current
/// end-to-end reserve clock, the only figure was a *derived* 4600 ms in the host plant: a sum of two
/// upshift deadlines describing a two-segment shape a downshift did not have. Those old deadlines
/// excluded `control.prime`, the master playlist and both `hls_cursor_next` calls. The live upshift
/// now carries one playhead-funded reserve through those control legs and the first candidate-media
/// phase, while this trace records their separate wall costs on every exit. A setup-bearing object
/// and its optional ordinary continuation remain staged under that same grant until one verdict.
///
/// **Why the drawdown is otherwise invisible.** The prime arm runs INLINE inside the loop that
/// emits one `abr: sample` per iteration, so no sample is emitted between proposal and commit. A
/// `min_buf_ms` computed from `abr: sample` cannot see the transaction's cost at all; that is a
/// property of where the samples are taken, not evidence that the cost is small.
struct TxTrace {
    started: std::time::Instant,
    /// The control plane is THREE requests, not one near-zero-byte transfer: PMS session
    /// creation, the master playlist and the media playlist. Logged separately because they
    /// scale with different things — `prime` is server work and is anti-correlated with link
    /// speed, while the two playlist fetches are round trips. A single `control=` total cannot
    /// tell a slow encoder registration from a distant server, and the transaction's fixed
    /// overhead is estimated from exactly this number.
    prime_done_ms: Option<i64>,
    master_done_ms: Option<i64>,
    control_plane_ms: Option<i64>,
    /// Acquisition of the structurally unique first object in the candidate session.
    warmup_acq_ms: Option<i64>,
    /// Acquisition of the first ordinary object from that same already-running encoder. Present
    /// only when the boundary object was funded but did not itself satisfy the steady law.
    graded_acq_ms: Option<i64>,
    buf_start_ms: Option<i64>,
    /// **The deadline the warm-up leg was actually granted, ms.** Without it a captured log cannot
    /// say whether a transaction was bounded — only how long it took — so "the deadline held" is
    /// not a statement the corpus can make about itself. `None` on every exit path that never
    /// reached the media fetch.
    warmup_dl_ms: Option<i64>,
    /// Acquisition of the current stream's segment immediately before the transaction. Retained
    /// as transaction context; the live exploration budget itself comes from the full bag runway.
    cur_acq_before_ms: i64,
    net_kbps: u32,
    fast_kbps: u32,
    slow_kbps: u32,
    unc_pm: u32,
    direction: crate::abr::Direction,
    from_kbps: u32,
    to_kbps: u32,
    outcome: &'static str,
    /// Elapsed at the moment the commit/reject was DECIDED. `total` below runs to scope end and so
    /// can also contain the single post-decision feed of every staged candidate object.
    decided_ms: Option<i64>,
    /// The reserve at the decision. This precedes every candidate feed.
    buf_decided_ms: Option<i64>,
    /// Sum of every candidate feed phase. Feeding can block on `aq_push` against a full queue;
    /// this is BACKPRESSURE, not acquisition cost: the media is already in hand and replenishes
    /// the reserve as it lands. It used to be folded into `decided_ms` — which is why the first
    /// published figure for an upshift was 9563 ms against a true 3065 ms.
    feed_ms: Option<i64>,
    /// The reserve after the feed, so the pre/post pair brackets what the staged segments added.
    buf_fed_ms: Option<i64>,
    /// The optional ordinary candidate acquisition after a setup-bearing boundary. It remains
    /// absent when the boundary object itself satisfies the steady conservation law.
    graded_bytes: Option<u64>,
    /// The completed acquisition on which the final candidate verdict rests: the boundary object
    /// when it is directly admissible, otherwise the staged ordinary object. The offline
    /// conservation grader needs all three facts because a reset otherwise hides that `(A_i,D_i)`
    /// observation.
    candidate_acq_ms: Option<i64>,
    candidate_bytes: Option<u64>,
    candidate_dur_ms: Option<i64>,
    /// The candidate rendition's DECLARED rate, kbit/s, from its own master playlist. `None`
    /// (logged `-1`) on every exit path that never got that far -- which is not zero and must not
    /// read as a rendition that declares nothing.
    declared_kbps: Option<u32>,
}

/// Lifetime marker for one exploratory upshift. The transaction's own deadline limits its spend
/// to `B-max(R_s,D_next)`; the published retained reserve is diagnostic and prevents an already-held
/// clock from restarting mid-transaction. It is not a second main-thread pause threshold.
struct HlsTrialReserve {
    token: u64,
}

impl HlsTrialReserve {
    fn arm(runway_ms: i64, expected_user_sequence: u64) -> Option<Self> {
        SHARED
            .arm_hls_trial_at(runway_ms, expected_user_sequence)
            .map(|token| Self { token })
    }

    fn token(&self) -> u64 {
        self.token
    }
}

impl Drop for HlsTrialReserve {
    fn drop(&mut self) {
        // A teardown may already have revoked this generation. A stale drop must never clear a
        // newer session's trial reserve, which is why finish is token-checked.
        let _ = SHARED.finish_hls_trial(self.token);
    }
}

/// Exclusive ownership of one automatic actuator while its blocking PMS/native work runs outside
/// the playback-clock mutex.  Acquisition and commit authorization are synchronized state-machine
/// transitions; dropping the lease is the single rollback/finish edge on every return path.
struct HlsAutomaticLease {
    token: u64,
}

impl HlsAutomaticLease {
    fn begin(
        kind: crate::player::HlsAutomaticTransition,
        expected_user_sequence: u64,
    ) -> Option<Self> {
        SHARED
            .begin_hls_automatic_transition_at(kind, expected_user_sequence)
            .map(|token| Self { token })
    }

    /// Linearization point for a route-changing Original result.  A user hold or native-clock
    /// transition which won the mutex first makes the result retainable evidence, not an action.
    fn authorize_commit(&self) -> bool {
        SHARED.authorize_hls_automatic_transition_commit(self.token)
    }

    fn token(&self) -> u64 {
        self.token
    }
}

impl Drop for HlsAutomaticLease {
    fn drop(&mut self) {
        // Session teardown revokes every outstanding token before joining the worker; in that
        // ordering `false` is the expected proof that this stale completion owns nothing.
        let _ = SHARED.finish_hls_automatic_transition(self.token);
    }
}

/// Fence the irreversible half of a candidate transition: arm immediately before the first
/// candidate AU may be queued, then hold until controller/route ownership has committed or the old
/// cursor has been realigned after a recoverable rejection. While armed, the main-thread pump
/// cannot resume an internal clock from candidate tails combined with the previous actuator's
/// recovery epoch.
///
/// Armed is deliberately fail-closed: an ordinary drop, including unwinding, leaves the generation
/// odd until queue teardown resets it. Only [`Self::settle`] may release the fence, and callers do
/// that only after the candidate commits or the old cursor is proven realigned with every published
/// candidate AU.
struct HlsCandidateTransition {
    generation: u64,
}

impl HlsCandidateTransition {
    fn arm(automatic_token: u64) -> Result<Self, HlsExit> {
        Self::arm_for(Some(automatic_token))
    }

    #[cfg(test)]
    fn arm_unowned_for_test() -> Result<Self, HlsExit> {
        Self::arm_for(None)
    }

    fn arm_for(automatic_token: Option<u64>) -> Result<Self, HlsExit> {
        match SHARED.begin_hls_candidate_transition(automatic_token) {
            Ok(generation) => Ok(Self { generation }),
            Err(crate::player::HlsClockFenceError::Stopping) => Err(HlsExit::Aborted),
            Err(crate::player::HlsClockFenceError::Overlap) => {
                Err(HlsExit::Failed("overlapping HLS candidate publication"))
            }
            Err(crate::player::HlsClockFenceError::Exhausted) => {
                Err(HlsExit::Failed("HLS candidate generation exhausted"))
            }
            Err(crate::player::HlsClockFenceError::Superseded) => Err(HlsExit::Failed(
                "HLS candidate automatic lease was superseded",
            )),
        }
    }

    fn settle(self) {
        let settled = SHARED.settle_hls_candidate_transition(self.generation);
        debug_assert!(
            settled,
            "candidate generation changed inside its sole writer"
        );
    }
}

fn settle_candidate_transition(transition: &mut Option<HlsCandidateTransition>) {
    if let Some(transition) = transition.take() {
        transition.settle();
    }
}

/// A recovery downshift restores the picture; an upshift only explores quality.  Starting the
/// latter while the native clock is already held makes its private transaction latency part of
/// the visible rebuffer even though the recovery media is already in hand.  Order those two
/// operations by what they do: finish the active recovery edge first, then let ordinary measured
/// exploration run.  No duration or reserve threshold is involved.
fn hls_quality_trial_may_start(
    direction: crate::abr::Direction,
    rebuffer_requested: bool,
    rebuffering: bool,
    user_paused: bool,
) -> bool {
    !user_paused
        && (direction == crate::abr::Direction::Down || !(rebuffer_requested || rebuffering))
}

/// A source-mode recovery verdict is a higher-level actuator decision than an HLS rung proposal.
/// Once latched, it takes the next quiescent media boundary regardless of whether the rung
/// controller happened to answer `Stay` or `Prime` there. Returning the pending proposal makes
/// its cancellation explicit; otherwise it would survive into a source reload or, worse, let a
/// stale Original verdict cross a successful change of HLS operating point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LatchedOriginalRecovery {
    Wait,
    Switch {
        kbps: u32,
        cancel: Option<crate::abr::Proposal>,
    },
}

fn latched_original_recovery(
    recover_kbps: u32,
    fresh_probe_kbps: Option<u32>,
    decision: crate::abr::Decision,
    encoder_cleanup_ready: bool,
    buffered_ms: Option<i64>,
    media_duration_ms: u32,
) -> LatchedOriginalRecovery {
    // A fresh successful source observation is already sitting on a completed/fed HLS boundary.
    // It must be eligible on THIS call, not copied into a local latch which can only be consumed
    // after another HLS GET.  PMS may rebind the shared Streaming Resource to the raw Part as a
    // side effect of that probe; requiring one more old-session segment then makes the transition
    // depend on a resource the probe itself may have invalidated.
    let recover_kbps = fresh_probe_kbps.unwrap_or(recover_kbps);
    if recover_kbps == 0
        || !encoder_cleanup_ready
        || !buffered_ms.is_some_and(|ms| ms >= i64::from(media_duration_ms))
    {
        return LatchedOriginalRecovery::Wait;
    }
    LatchedOriginalRecovery::Switch {
        kbps: recover_kbps,
        cancel: match decision {
            crate::abr::Decision::Stay => None,
            crate::abr::Decision::Prime(proposal) => Some(proposal),
        },
    }
}

/// Publish the worker -> main-thread source handoff only while this worker still owns both the AU
/// queue and the active PMS route.  This is the source-mode twin of [`publish_hls_candidate`]:
/// teardown and publication linearize on AQ, then route identity is checked under ACTIVE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OriginalRecoveryPublication {
    Published,
    Busy,
    Aborted,
    RouteMoved,
    ClockMoved,
}

fn publish_original_recovery(
    aq: *mut AuQueue,
    control: &crate::route::HlsAbrControl,
    active_route: &crate::route::WorkerTicket,
    recover_kbps: u32,
    lease: &HlsAutomaticLease,
) -> OriginalRecoveryPublication {
    // This mutex decision is the source-transition linearization point. The route publication
    // below has its own ticket/serial state machine; a later user Pause is ordered after this
    // authorization and is therefore carried as viewer intent across the resulting reload.
    if !lease.authorize_commit() {
        return OriginalRecoveryPublication::ClockMoved;
    }
    crate::aq::aq_if_not_aborted(aq, || {
        match control.request_original_recovery(
            active_route,
            recover_kbps,
            SHARED.playpos_ns.load(Ordering::Relaxed).max(0),
        ) {
            crate::route::AutomaticIntentResult::Accepted => {
                SHARED.dg_abr_action.store(
                    crate::player::ABR_ACTION_RECOVER_ORIGINAL,
                    Ordering::Relaxed,
                );
                OriginalRecoveryPublication::Published
            }
            crate::route::AutomaticIntentResult::Busy => OriginalRecoveryPublication::Busy,
            crate::route::AutomaticIntentResult::Stale => OriginalRecoveryPublication::RouteMoved,
        }
    })
    .unwrap_or(OriginalRecoveryPublication::Aborted)
}

impl TxTrace {
    fn open(
        proposal: crate::abr::Proposal,
        from: crate::abr::Rung,
        sample: crate::abr::SegmentSample,
        delivery: &crate::abr::CapacityEstimate,
    ) -> Self {
        Self {
            started: std::time::Instant::now(),
            prime_done_ms: None,
            master_done_ms: None,
            control_plane_ms: None,
            warmup_acq_ms: None,
            graded_acq_ms: None,
            warmup_dl_ms: None,
            // The HONEST reserve: `hls_buffer_snapshot(None)` reads only what has actually been
            // FED, where the `Some(output)` arm folds in a staged candidate tail that has not been.
            buf_start_ms: hls_buffer_snapshot(None).buffered_ms(),
            cur_acq_before_ms: i64::from(sample.production_ratio_pm())
                * i64::from(sample.media_duration_ms())
                / 1_000,
            net_kbps: sample.network_kbps(),
            fast_kbps: delivery.fast_kbps,
            slow_kbps: delivery.slow_kbps,
            unc_pm: delivery.uncertainty_pm,
            direction: proposal.direction,
            from_kbps: from.kbps(),
            to_kbps: proposal.rung.kbps(),
            // If this survives to the log, an exit path forgot `finish` — the record
            // says so rather than looking like a labelled outcome.
            outcome: "UNLABELLED_PATH",
            decided_ms: None,
            buf_decided_ms: None,
            feed_ms: None,
            buf_fed_ms: None,
            graded_bytes: None,
            candidate_acq_ms: None,
            candidate_bytes: None,
            candidate_dur_ms: None,
            declared_kbps: None,
        }
    }

    fn mark_prime(&mut self) {
        self.prime_done_ms = Some(self.elapsed_ms());
    }

    /// The candidate's master playlist is in, so its DECLARED rate is known -- for the only
    /// moment in a playback at which any per-rung rate is knowable at all. Recorded here so the
    /// `abr: tx` line carries it beside the catalog's guess and the two can be differenced on a
    /// captured trace with no extra instrumentation.
    fn mark_master(&mut self, declared_bps: u64) {
        self.master_done_ms = Some(self.elapsed_ms());
        self.declared_kbps = Some(u32::try_from(declared_bps / 1_000).unwrap_or(u32::MAX));
    }

    fn mark_control_plane(&mut self) {
        self.control_plane_ms = Some(self.elapsed_ms());
    }

    fn mark_warmup_deadline(&mut self, budget: std::time::Duration) {
        self.warmup_dl_ms = Some(i64::try_from(budget.as_millis()).unwrap_or(i64::MAX));
    }

    /// Add the post-decision feed phase, measured from its own start. All staged candidate objects
    /// cross into playback together under the committed media owner.
    fn mark_feed(&mut self, started: std::time::Instant) {
        let elapsed = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        self.feed_ms = Some(self.feed_ms.unwrap_or(0).saturating_add(elapsed));
        self.buf_fed_ms = hls_buffer_snapshot(None).buffered_ms();
    }

    /// Acquisition of one candidate media segment, measured the same way production measures the
    /// current stream: request to demux complete, backpressure excluded.
    fn mark_media(&mut self, output: &HlsSegmentOutput, graded: bool) {
        let acq = i64::try_from(output.transfer.total_us / 1_000).unwrap_or(i64::MAX);
        if graded {
            self.graded_acq_ms = Some(acq);
            self.graded_bytes = Some(output.transfer.bytes);
        } else {
            self.warmup_acq_ms = Some(acq);
        }
    }

    fn mark_candidate_evidence(
        &mut self,
        output: &HlsSegmentOutput,
        duration: std::time::Duration,
    ) {
        self.candidate_acq_ms =
            Some(i64::try_from(output.transfer.total_us / 1_000).unwrap_or(i64::MAX));
        self.candidate_bytes = Some(output.transfer.bytes);
        self.candidate_dur_ms = Some(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX));
    }

    /// Freeze the user-visible decision cost before any post-decision queue backpressure.  This
    /// deliberately does NOT label the transaction committed: feeding the staged media and the
    /// controller's atomic reset+seed are still fallible after this point.
    fn mark_decided(&mut self) {
        if self.decided_ms.is_none() {
            self.decided_ms = Some(self.elapsed_ms());
            self.buf_decided_ms = hls_buffer_snapshot(None).buffered_ms();
        }
    }

    /// Label the path only when the outcome named here has really happened.  Rejection paths call
    /// this at their decision point; a commit first calls `mark_decided`, performs the fallible
    /// feed and atomic controller transition, and reaches this method only after the new finite
    /// bag has actually been seeded.
    fn finish(&mut self, outcome: &'static str) {
        self.mark_decided();
        self.outcome = outcome;
    }

    fn elapsed_ms(&self) -> i64 {
        i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX)
    }
}

impl Drop for TxTrace {
    fn drop(&mut self) {
        let opt = |v: Option<i64>| {
            v.map(|n| n.to_string())
                .unwrap_or_else(|| "none".to_string())
        };
        // The three control-plane legs as DURATIONS, each measured from the end of the one before
        // it, so they sum to `control=` and none of them silently contains another.
        let leg = |to: Option<i64>, from: Option<i64>| match (to, from) {
            (Some(to), Some(from)) => Some(to.saturating_sub(from)),
            _ => None,
        };
        crate::player::log(&format!(
            "abr: tx {:?} {}->{}kbps outcome={} decided={}ms total={}ms control={}ms \
             prime={}ms master={}ms media={}ms warmup={}ms \
             graded={}ms warmup_dl={}ms buf_start={}ms buf_decided={}ms feed={}ms buf_fed={}ms \
             buf_end={}ms \
             cur_acq_before={}ms net={}kbps fast={}kbps slow={}kbps unc={}pm declared={}kbps \
             graded_bytes={} candidate_acq={}ms candidate_bytes={} candidate_dur={}ms",
            self.direction,
            self.from_kbps,
            self.to_kbps,
            self.outcome,
            opt(self.decided_ms),
            self.elapsed_ms(),
            opt(self.control_plane_ms),
            opt(self.prime_done_ms),
            opt(leg(self.master_done_ms, self.prime_done_ms)),
            opt(leg(self.control_plane_ms, self.master_done_ms)),
            opt(self.warmup_acq_ms),
            opt(self.graded_acq_ms),
            opt(self.warmup_dl_ms),
            opt(self.buf_start_ms),
            opt(self.buf_decided_ms),
            opt(self.feed_ms),
            opt(self.buf_fed_ms),
            opt(hls_buffer_snapshot(None).buffered_ms()),
            self.cur_acq_before_ms,
            self.net_kbps,
            self.fast_kbps,
            self.slow_kbps,
            self.unc_pm,
            self.declared_kbps.map(i64::from).unwrap_or(-1),
            self.graded_bytes.map(|b| b as i64).unwrap_or(-1),
            opt(self.candidate_acq_ms),
            self.candidate_bytes.map(|b| b as i64).unwrap_or(-1),
            self.candidate_dur_ms.unwrap_or(-1),
        ));
    }
}

/// **`cause` is a judgement each call site makes about its own failure, and it is load-bearing.**
///
/// N11's backoff refuses to re-prime a rung that just failed. That is right when the failure was
/// ABOUT the rung — a completed-but-unfunded response, PMS refusing this rung's ceiling
/// (`route::PrimeRefusal::Rung`), an acceptance test the candidate did not pass — and wrong when it
/// was about the session: `reserve_unreadable` is the audio lane falling silent at a seek,
/// `origin_changed` is the route moving underneath, and `prime_session_moved` is either of those
/// caught one branch earlier, before a round trip was spent. **"A refused prime" is about the rung
/// only when the server actually refused it**, which is why `prime` reports which of its four exits
/// it took rather than handing back a bare failure for this function to guess at.
/// A missed absolute deadline is a third class: it is common censored budget evidence, because no
/// complete response exists whose size could order this rung against another.
/// Blocking a good rung for either would be the guard doing harm in the one direction that has no
/// recovery path, so the classification lives here, beside the `tx.finish` string that already
/// names the same event for the transaction log.
fn reject_hls_abr(
    controller: &mut crate::abr::Controller,
    proposal: crate::abr::Proposal,
    cause: crate::abr::RejectCause,
    now_ms: u64,
) {
    let refreshing_same_request = proposal.rung == controller.current();
    controller.reject(proposal, cause, now_ms);
    publish_hls_abr_action(proposal, refreshing_same_request, Some(false));
}

/// Candidate rejection after a transaction has actually run. Freeze the transaction's decision
/// snapshot before publishing the controller result so the tx diagnostic and action describe the
/// same instant.
fn reject_hls_abr_after_transaction(
    controller: &mut crate::abr::Controller,
    proposal: crate::abr::Proposal,
    cause: crate::abr::RejectCause,
    tx: &mut TxTrace,
    now_ms: u64,
) {
    // Freeze one reserve snapshot and use it for both the controller certificate and the tx log.
    // Two adjacent live reads can differ while playback advances and would make the diagnostic
    // line disagree with the quantity that actually released the next experiment.
    tx.mark_decided();
    let refreshing_same_request = proposal.rung == controller.current();
    controller.reject(proposal, cause, now_ms);
    publish_hls_abr_action(proposal, refreshing_same_request, Some(false));
}

/// Every ownership transition a fully-fed candidate can take.
///
/// This is synchronized with teardown by the video AU queue's abort mutex, not merely sampled
/// before publication. `Active` transfers the candidate to the route and hands the previous
/// encoder back to the caller as an explicit cleanup obligation. No losing arm changes controller,
/// route or worker-local encoder, so the transaction still owns and must abandon its candidate.
#[derive(Debug, PartialEq, Eq)]
enum CandidatePublication {
    Aborted,
    SessionEnded,
    RouteMoved,
    ControllerRejected,
    Active { previous: String },
}

fn publish_hls_candidate(
    aq: *mut AuQueue,
    control: &crate::route::HlsAbrControl,
    controller: &mut crate::abr::Controller,
    active_route: &mut crate::route::WorkerTicket,
    candidate: &crate::route::PrimedHls,
    proposal: crate::abr::Proposal,
    sample: crate::abr::SegmentSample,
    variant: crate::abr::ObservedHlsVariant,
    commit_now_ms: u64,
) -> CandidatePublication {
    // The route comparison and the worker-local replacement describe the same transition, but
    // borrowing the live lease for both would also make Rust model them as overlapping reads and
    // writes. Freeze the expected owner before entering either serialization boundary; the route
    // still validates it while holding ACTIVE, and only the accepted callback mutates the local
    // owner.
    let expected_route = active_route.clone();
    crate::aq::aq_if_not_aborted(aq, || {
        match control.commit_transition(
            &expected_route,
            candidate,
            proposal,
            (variant, sample.network_kbps()),
            || {
                controller
                    .commit_candidate(proposal, sample, variant, commit_now_ms)
                    .then(|| expected_route.encoder().to_owned())
            },
        ) {
            Ok((previous, replacement_route)) => {
                *active_route = replacement_route;
                CandidatePublication::Active { previous }
            }
            Err(crate::route::HlsCommitRefusal::Session) => CandidatePublication::SessionEnded,
            Err(crate::route::HlsCommitRefusal::RouteMoved) => CandidatePublication::RouteMoved,
            Err(crate::route::HlsCommitRefusal::TransitionRejected) => {
                CandidatePublication::ControllerRejected
            }
        }
    })
    .unwrap_or(CandidatePublication::Aborted)
}

/// Translate a transport exit without pretending every failed request says something about the
/// rendition. Only an exhausted physical deadline is censored candidate evidence. Abort, parse,
/// HTTP and session failures are circumstances; replaying them may be undesirable operationally,
/// but a larger reserve does not logically validate or invalidate the actuator.
fn abr_reject_for_hls_exit(exit: HlsExit) -> crate::abr::RejectCause {
    match exit {
        HlsExit::PrimeExpired | HlsExit::StallAbort(_) => crate::abr::RejectCause::Censored,
        HlsExit::Aborted | HlsExit::NotReady | HlsExit::Failed(_) => {
            crate::abr::RejectCause::Circumstance
        }
    }
}

/// Classify a completed candidate from what its RESPONSE proved, never from the request label.
/// A no-gain response is PMS underfill: even when the request actuator differs, its smaller output
/// does not order that actuator against lower request ceilings. Treating it as structural walked
/// the whole ladder and allocated one physical encoder per rung on the remote PMS.
fn abr_reject_for_candidate_result(
    raster_ready: bool,
    quality_improves: bool,
    verdict: crate::abr::CandidateVerdict,
) -> crate::abr::RejectCause {
    if !raster_ready {
        return crate::abr::RejectCause::StructuralAtOrBelow;
    }
    if !quality_improves {
        return crate::abr::RejectCause::ResponseUnchanged;
    }
    match verdict {
        // A setup-bearing verdict is consumed by the serial continuation path before rejection.
        // If a future caller leaks it here, retain only an actuator-specific funding frontier;
        // it is explicitly not steady `A>D` evidence.
        crate::abr::CandidateVerdict::SetupBearing => crate::abr::RejectCause::Candidate,
        crate::abr::CandidateVerdict::Unsustainable => {
            crate::abr::RejectCause::CompletedUnsustainable
        }
        crate::abr::CandidateVerdict::Unfunded => crate::abr::RejectCause::Candidate,
        crate::abr::CandidateVerdict::Incomplete | crate::abr::CandidateVerdict::ReserveUnknown => {
            crate::abr::RejectCause::Circumstance
        }
        crate::abr::CandidateVerdict::Ready => {
            unreachable!("a ready candidate can only be rejected by response geometry")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsCandidateDisposition {
    Commit,
    Discard {
        cause: crate::abr::RejectCause,
        outcome: &'static str,
    },
}

fn hls_candidate_requires_rung_box(
    proposal: crate::abr::Proposal,
    reserve_policy: crate::abr::ReservePolicy,
) -> bool {
    !(proposal.direction == crate::abr::Direction::Down
        && proposal.rung.at_floor()
        && reserve_policy == crate::abr::ReservePolicy::TerminalFloor)
}

fn hls_candidate_disposition(
    raster_ready: bool,
    quality_improves: bool,
    verdict: crate::abr::CandidateVerdict,
) -> HlsCandidateDisposition {
    if raster_ready && quality_improves && verdict == crate::abr::CandidateVerdict::Ready {
        HlsCandidateDisposition::Commit
    } else {
        let outcome = if !raster_ready {
            "raster_refused"
        } else if !quality_improves {
            "no_quality_gain"
        } else {
            "not_ready_discarded"
        };
        HlsCandidateDisposition::Discard {
            cause: abr_reject_for_candidate_result(raster_ready, quality_improves, verdict),
            outcome,
        }
    }
}

fn original_probe_observation(
    probe: crate::curlio::ThroughputSample,
) -> crate::abr::CapacityObservation {
    crate::abr::CapacityObservation {
        kbps: u32::try_from(probe.kbps()).unwrap_or(u32::MAX),
        bytes: u64::try_from(probe.bytes).unwrap_or(0),
        active_us: u64::try_from(probe.elapsed.as_micros()).unwrap_or(u64::MAX),
        completed: probe.target_reached,
    }
}

fn hls_demux(
    origin: &crate::plex::Origin,
    path: &str,
    acodec: &str,
    abr: Option<(crate::route::HlsAbrControl, crate::route::WorkerTicket)>,
    aq: *mut AuQueue,
    aqa: *mut AuQueue,
    hs: *mut HttpStream,
) -> Result<(), HlsExit> {
    // The change detector is process-global, so a second playback of the same stream would
    // otherwise log nothing at all. Cleared here rather than on teardown: this is the one place
    // that runs exactly once per HLS demux, whatever ended the previous one.
    HLS_LAST_VIDEO.store(u64::MAX, Ordering::Relaxed);
    if let Some((control, _)) = abr.as_ref() {
        SHARED
            .dg_abr_mode
            .store(crate::player::ABR_MODE_HLS, Ordering::Relaxed);
        SHARED
            .dg_abr_kbps
            .store(i64::from(control.initial_rung.kbps()), Ordering::Relaxed);
        SHARED.dg_abr_declared_kbps.store(-1, Ordering::Relaxed);
        SHARED.dg_abr_media_kbps.store(-1, Ordering::Relaxed);
        SHARED.dg_abr_net_kbps.store(-1, Ordering::Relaxed);
        SHARED.dg_abr_buffer_ms.store(-1, Ordering::Relaxed);
        SHARED.dg_abr_ratio_pm.store(-1, Ordering::Relaxed);
        SHARED
            .dg_abr_action
            .store(crate::player::ABR_ACTION_STEADY, Ordering::Relaxed);
        SHARED.dg_abr_target_kbps.store(0, Ordering::Relaxed);
        SHARED.dg_abr_unsafe_deficit_ms.store(0, Ordering::Relaxed);
    }
    let mut cursor = hls_cursor_open(origin, path, aq, hs, true, None)?;
    if abr.is_some() {
        SHARED.dg_abr_declared_kbps.store(
            i64::try_from(cursor.declared_bps / 1_000).unwrap_or(i64::MAX),
            Ordering::Relaxed,
        );
    }
    let mut timeline = crate::hls::SegmentTimeline::default();
    // The origin of the visible-switch decay clock: the main thread's `since_last_ms` capture
    // is a value AS OF NOW, and this is now. `OriginalRecovery::advance_to` in the segment loop
    // moves it forward from here.
    let history_since = std::time::Instant::now();
    // **The controller's ONE clock, named once.** `Controller::observe`/`commit`/`reject` share a
    // monotonic origin so logs and switch-history decay describe the same transaction. Failure
    // release is not time-based: it reads the newly observed surplus, and waiting alone changes
    // no certificate.
    let now_ms = || history_since.elapsed().as_millis() as u64;
    let mut adaptive = abr.map(|(control, encoder)| {
        let initial = control.initial_rung;
        let catalog = control.catalog;
        let prior = control.prior;
        // The main thread's capture is the STARTING point and this is the moment it was taken,
        // so 0 is correct HERE and was never the defect. The defect was that it stayed 0: the
        // history was frozen into the recovery gate at construction and nothing advanced it
        // again, so the visible-switch penalty never decayed and Original stayed unreachable for
        // the rest of the playback after two mode switches. `recovery.advance_to` in the segment
        // loop below is the clock; this line is only its origin.
        let advanced_ms: u64 = 0;
        crate::player::log(&format!(
            "abr: history switches={} since_last={} advanced={}ms",
            control.history.visible_switches,
            control
                .history
                .since_last_ms
                .map(|ms| ms.to_string())
                .unwrap_or_else(|| "none".to_string()),
            advanced_ms,
        ));
        // **Say it when the recovery does not exist at all.** `abr: probe withheld` is emitted
        // from inside `probe_due`, which lives inside the gate — so a gate that was never built
        // produces a log byte-identical to a healthy playback that simply never needed Original,
        // and there is no third state to tell them apart. Device, 2026-08-30: ~1 600 lines with
        // no probe, no reason and no `abr: mode`, and the only way anyone found it was by
        // noticing what was ABSENT. One line at construction is the whole fix for that.
        if !control.can_recover_original() {
            crate::player::log(&format!(
                "abr: no Original recovery for this playback (candidate={} source={}kbps) — \
                 HLS is the ceiling until the next resolve",
                u8::from(control.has_original_candidate()),
                control.original_source_kbps(),
            ));
        }
        let recovery = control
            .can_recover_original()
            .then(|| {
                crate::abr::OriginalRecovery::new(
                    control.original_source_kbps(),
                    crate::abr::AbrPolicy::measured(),
                    control.original_features,
                    // The worker's own clock takes over from here; the main thread's capture is
                    // the starting point, not a live view.
                    control.history.advanced_by(advanced_ms),
                    // This playback's actuator set, so the recovery comparison scores the rungs
                    // that exist here rather than a synthetic one — and so Original's own quality
                    // can be scored against the SOURCE raster the catalog was bounded by.
                    control.catalog,
                )
            })
            .flatten();
        // **Observation only (plan I0-G).** A seek builds a FRESH controller here, so the live
        // link estimate does not survive it — the only thing that does is `prior`, whose writer on
        // the fallback path is the rate measured at the moment Original failed. One line, printed
        // where the re-seed happens, is what turns that from an argument about source into a
        // before/after a device run can show. `abr: steady` on either side of the seek carries the
        // matching slow/fast/unc/n. Nothing is repaired here; that is increment I8.
        let pin = crate::dev::abr_pin();
        let mut controller =
            crate::abr::Controller::starting_at(initial, prior, catalog).pinned_to(pin);
        if let Some((variant, evidence_kbps)) = control.initial_observed {
            controller.observe_active_variant(variant, evidence_kbps);
        }
        let seeded = controller.telemetry();
        crate::player::log(&format!(
            "abr: seed rung={}kbps prior={} slow={}kbps fast={}kbps unc={}pm n={} pin={}",
            initial.kbps(),
            prior
                .map(|p| format!("{}kbps", p.slow_kbps))
                .unwrap_or_else(|| "none".to_string()),
            seeded.delivery.slow_kbps,
            seeded.delivery.fast_kbps,
            seeded.delivery.uncertainty_pm,
            seeded.delivery.samples,
            pin.map(|r| format!("{}kbps", r.kbps()))
                .unwrap_or_else(|| "none".to_string()),
        ));
        (
            control, encoder, controller, recovery,
            // A latched `Recover` verdict, waiting for a quiescent segment to act on.
            0u32,
        )
    });
    // Consume native-accepted pause EVENTS, not a sampled bool. The worker can spend a complete
    // Pause→Resume cycle blocked in aq_push/network I/O and must still age the first post-resume
    // estimate by the exact cumulative interval.
    let mut pause_cursor =
        crate::player::UserPauseCursor::new(crate::player::TX.pause_state_sample());
    // Last-logged recovery refusal, so a steady one costs one line instead of one per segment.
    // Lives here rather than on `OriginalRecovery` because it is a property of the LOG, not of the
    // gate — and the gate is rebuilt on a seek while this loop is not.
    let mut last_probe_block: Option<crate::abr::ProbeBlock> = None;
    // PMS acknowledges a transcode stop before its worker finishes either half of the lifecycle.
    // The route ledger drives physical ping then exact logical-resource close; this latch keeps a
    // potentially long server cleanup to one waiting line and one release line.
    let mut cleanup_wait_logged = false;
    // Exactly one completed object in a fresh cursor carries PMS encoder/session startup. It is
    // buffered and measured, but the repeatable acquisition window begins with the next object;
    // see `Controller::observe_session_boundary`.
    let mut session_boundary_pending = adaptive.is_some();

    'segments: while let Some(segment) = hls_cursor_next(&mut cursor, aq, hs, None)? {
        let mut clock = timeline
            .begin(segment.duration)
            .map_err(|_| HlsExit::Failed("HLS content timeline overflow"))?;
        // **Arm the terminal guard only while the clock is running.** During an existing internal
        // hold the response is rebuilding reserve, not spending it, and abandoning that response
        // would make the depleted state absorbing. An unreadable reserve has no boundary to arm.
        // Above the floor, a later physical B=0 hold abandons the oversized object; at the floor
        // the same event keeps reading because no cheaper response exists.
        let stall_guard = adaptive.as_ref().and_then(|state| {
            arm_active_stall_guard(
                hls_buffer_snapshot(None).buffered_ms(),
                state.2.current().at_floor(),
                SHARED.hls_rebuffering.load(Ordering::Acquire),
            )
        });
        // **An abort is not a failure and not a delivery — it is the observed terminal boundary
        // arriving instead of a segment.** The segment goes back on the cursor unconsumed,
        // nothing is fed and the content timeline does not advance (we delivered no media); what
        // continues is the ordinary censored-event -> transact path below. The controller only
        // ever decides between segments, so the fetch that already exhausted the picture's
        // reserve is converted into the one thing it was withholding: a recovery decision.
        //
        // **What it must NOT do is enter the capacity estimate as a completed transfer.** The
        // response is incomplete by construction, so its bytes may be a receive-buffer burst or
        // a PMS JIT production interval rather than path service — device-measured
        // `bytes=1448 ... at 42277kbps` while the shaper held the link at 500 kbps. Entered as
        // complete it kept `conservative_kbps` near 16 Mbit/s, so every correct downshift picked a
        // target 30x too dear. See `SegmentSample::abandoned`.
        let mut fetch_abandoned = false;
        let output = match unsafe {
            hls_demux_segment(
                &segment,
                &cursor.auth,
                &mut clock,
                aq,
                hs,
                acodec,
                ReserveDeadlineState::new(None, false),
                stall_guard,
            )
        } {
            Ok(output) => output,
            Err(HlsExit::StallAbort(transfer)) => {
                crate::player::log(&format!(
                    "abr: stall abort seq={} bytes={} active={}ms reserve={}ms censored=1 \
                     cause=terminal_reserve — abandoning the incomplete fetch so a cheaper rung \
                     can be decided",
                    segment.sequence,
                    transfer.bytes,
                    transfer.active_us / 1_000,
                    stall_guard.map(|g| g.reserve_ms_at_start).unwrap_or(-1),
                ));
                cursor.pending.push_front(segment.clone());
                fetch_abandoned = true;
                HlsSegmentOutput {
                    aus: Vec::new(),
                    transfer,
                    video_width: 0,
                    video_height: 0,
                    video_tail_ns: -1,
                    audio_tail_ns: None,
                }
            }
            Err(other) => return Err(other),
        };
        let delivered = !output.aus.is_empty();
        if delivered {
            hls_feed_segment(&output, aq, aqa)?;
            // A recovery epoch accepts only complete media from the stream that owns playback.
            // Candidate acquisitions are deliberately excluded below: until commit they are not
            // the process that will have to sustain the resumed clock.
            SHARED.observe_hls_recovery(output.transfer.total_us, segment.duration);
            timeline.commit(clock);
        }

        let Some((control, active_encoder, controller, recovery, recover_kbps)) = adaptive.as_mut()
        else {
            continue;
        };
        // ABSOLUTE elapsed, not a delta, so an irregular segment cadence cannot skew the decay.
        if let Some(gate) = recovery.as_mut() {
            gate.advance_to(now_ms());
        }
        let Some(mut sample) = hls_segment_sample(&output, segment.duration).map(|s| {
            if fetch_abandoned {
                s.abandoned()
            } else {
                s
            }
        }) else {
            crate::player::log("abr: ignoring invalid segment timing sample");
            continue;
        };
        if delivered {
            if let Some(variant) = crate::abr::ObservedHlsVariant::new(
                cursor.declared_bps,
                output.video_width,
                output.video_height,
            ) {
                controller.observe_active_variant(variant, sample.network_kbps());
                control.observe_active_variant(active_encoder, variant, sample.network_kbps());
            }
        }
        // One state-driven cleanup observation per completed HLS quantum at most. It performs no
        // network I/O on this demux thread: the route coalesces a background physical ping and
        // exact logical-resource close, and releases only after both are reconciled. The snapshot
        // governs all quality/source experiments decided from THIS sample; an asynchronous release
        // is consumed next segment.
        let encoder_cleanup_ready = if delivered && sample.completed() {
            control.observe_encoder_cleanup()
        } else {
            control.encoder_cleanup_ready()
        };
        if !encoder_cleanup_ready && !cleanup_wait_logged {
            cleanup_wait_logged = true;
            crate::player::log(
                "abr: waiting for PMS to release the stopped encoder before another experiment",
            );
        } else if encoder_cleanup_ready && cleanup_wait_logged {
            cleanup_wait_logged = false;
            crate::player::log("abr: PMS encoder release observed; exploration may resume");
        }
        // A pause between two segments really is unmeasured wall-clock time. Age the estimate by
        // it rather than letting a rate measured before the interruption decide what happens after.
        let pause_before_lookup = crate::player::TX.pause_state_sample();
        if let Some(elapsed) = pause_cursor.consume_completed(pause_before_lookup) {
            let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
            controller.on_resume(elapsed_ms);
            crate::player::log(&format!(
                "abr: consumed accepted pause sequence={} elapsed={}ms; delivery estimate aged",
                pause_before_lookup.sequence, elapsed_ms,
            ));
        }
        // The rollback law protects the NEXT object on the old cursor. Parse that exact EXTINF
        // before the controller grants any private quality/source transaction; substituting the
        // object which just completed is unsound for variable-duration HLS. A user-held playback
        // starts no automatic experiment, so it needs no speculative playlist refresh here.
        let rollback_media_ms = if pause_before_lookup.paused {
            None
        } else {
            hls_cursor_next_duration_ms(&mut cursor, aq, hs)?
        };
        // The exact next-object lookup above may refresh a live playlist and wait.  Its `D_next`
        // still belongs to the rollback cursor, and this sample's A/D/bytes still belong to the
        // completed object, but the reserve is a decision-time fact.  Re-read it after the wait so
        // neither the rung controller nor Original admission spends buffer which elapsed while
        // discovering the obligation.  Include `output`: its exact segment-end tail is newer than
        // the last AU PTS stored in SHARED and must not be rounded back merely by refreshing B.
        sample = sample.with_buffer(hls_buffer_snapshot(Some(&output)));
        // A complete Pause -> Resume may have occurred wholly inside the playlist refresh above.
        // Consume it before this sample can act, and name the synchronized HLS user sequence that
        // every later lease must still match.  Returning to `Running` is not the same boundary.
        let pause_state = crate::player::TX.pause_state_sample();
        if let Some(elapsed) = pause_cursor.consume_completed(pause_state) {
            let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
            controller.on_resume(elapsed_ms);
            crate::player::log(&format!(
                "abr: consumed accepted pause sequence={} elapsed={}ms after rollback lookup; delivery estimate aged",
                pause_state.sequence, elapsed_ms,
            ));
        }
        let paused_now = pause_state.paused;
        let expected_user_sequence = SHARED.hls_user_sequence();
        publish_hls_abr_sample(sample);
        let remaining_ms = remaining_playback_ms();
        let boundary = session_boundary_pending && delivered && sample.completed();
        let decision = if boundary {
            session_boundary_pending = false;
            crate::player::log(
                "abr: fresh-session boundary buffered; waiting for the first repeatable HLS acquisition",
            );
            controller.observe_session_boundary(sample, rollback_media_ms, now_ms())
        } else {
            controller.observe_with_rollback(sample, rollback_media_ms, now_ms())
        };
        if let Some(runway_ms) = controller.prime_runway_ms() {
            // The finite-bag replay boundary supersedes the single latest acquisition after its
            // media has been credited to the queues. It is available from the first completion
            // and uses the whole current bag: this controls when a paused clock may start, never
            // acts as a static mid-fetch stop threshold, and predicts no unseen draw.
            SHARED
                .hls_prime_runway_ms
                .store(runway_ms, Ordering::Release);
        }
        // ONE telemetry read for both: the panel must never show a safe budget from this segment
        // beside a risk score from the next. The MODEL is published whatever was decided — the
        // segments worth looking at are the ones that decided to move — while the `abr: steady`
        // LINE stays on the do-nothing path, because that is what it says happened.
        let telemetry = controller.telemetry();
        publish_hls_abr_model(&telemetry);
        log_hls_abr_sample(&telemetry, sample, decision);
        log_hls_abr_window(&telemetry, sample);
        // An upward candidate commit invalidates the HLS half of a terminal Original comparison,
        // not the completed source measurement. The first ordinary completed object from the new
        // live stream is the linearization boundary at which both sides are observable again.
        // Re-score here before either an HLS proposal or the source latch is acted on; no source
        // request and no probe counter are spent. From the same terminal state, a downward commit
        // clears its old-regime source evidence instead and therefore never enters this branch.
        if delivered && sample.completed() {
            let current_candidate = controller.catalog().candidate(controller.current());
            let reconsidered = recovery.as_mut().and_then(|gate| {
                gate.reconsider_after_hls_commit(
                    current_candidate,
                    &telemetry.production,
                    telemetry.buffer,
                    &telemetry.delivery,
                    remaining_ms,
                )
            });
            if let Some(reconsidered) = reconsidered {
                *recover_kbps = if reconsidered.verdict == crate::abr::RecoveryVerdict::Recover {
                    reconsidered.measured_kbps.max(1)
                } else {
                    0
                };
                last_probe_block = None;
                if let Some(cmp) = recovery.as_ref().and_then(|gate| gate.comparison()) {
                    log_original_mode_comparison(cmp);
                }
                crate::player::log(&format!(
                    "abr: retained Original probe reconsidered after HLS commit measured={}kbps \
                     verdict={:?}",
                    reconsidered.measured_kbps, reconsidered.verdict,
                ));
            }
        }
        if matches!(decision, crate::abr::Decision::Stay) {
            // Action and Reason are one current decision in the diagnostics panel. Leaving the
            // previous commit sticky while publishing this segment's RejectBackoff reason produced
            // the impossible photographed pair "changed up to 20 Mbps · retrying shortly". A
            // discrete probe/prime below may replace this again in the same iteration.
            SHARED
                .dg_abr_action
                .store(crate::player::ABR_ACTION_STEADY, Ordering::Relaxed);
            SHARED.dg_abr_target_kbps.store(0, Ordering::Relaxed);
            log_hls_abr_steady(&telemetry, remaining_ms);
        }

        // The switch itself needs a quiescent session and one media quantum to tear down against.
        // A `Prime` answer here has not started its transaction yet; the already-latched source
        // decision therefore cancels that pending rung explicitly instead of becoming stale
        // across a new operating point.
        if !paused_now {
            if let LatchedOriginalRecovery::Switch { kbps, cancel } = latched_original_recovery(
                *recover_kbps,
                None,
                decision,
                encoder_cleanup_ready,
                sample.buffer.buffered_ms(),
                sample.media_duration_ms(),
            ) {
                if let Some(lease) = HlsAutomaticLease::begin(
                    crate::player::HlsAutomaticTransition::Original,
                    expected_user_sequence,
                ) {
                    if let Some(proposal) = cancel {
                        reject_hls_abr(
                            controller,
                            proposal,
                            crate::abr::RejectCause::Circumstance,
                            now_ms(),
                        );
                    }
                    match publish_original_recovery(aq, control, active_encoder, kbps, &lease) {
                        OriginalRecoveryPublication::Published => {
                            crate::player::log(&format!(
                                "abr: source sustainable again at {kbps}kbps; requesting Original",
                            ));
                            break;
                        }
                        OriginalRecoveryPublication::Busy => {
                            crate::player::log(
                                "abr: Original recovery retained behind another route transition",
                            );
                        }
                        OriginalRecoveryPublication::ClockMoved => {
                            crate::player::log(
                                "abr: Original recovery retained after playback clock moved",
                            );
                        }
                        OriginalRecoveryPublication::Aborted
                        | OriginalRecoveryPublication::RouteMoved => return Err(HlsExit::Aborted),
                    }
                }
            }
        }

        if decision == crate::abr::Decision::Stay && !paused_now {
            let current_candidate = controller.catalog().candidate(controller.current());
            let buffer = controller.buffer();
            let delivery = controller.delivery();
            let verdict = recovery.as_mut().map(|gate| {
                gate.probe_due_with_rollback(
                    current_candidate,
                    controller.hls_frontier_exhausted(),
                    &telemetry.production,
                    sample,
                    rollback_media_ms,
                    controller.prime_runway_ms(),
                    buffer,
                    &delivery,
                    remaining_ms,
                    now_ms(),
                )
            });
            // **Say why NOT, once per distinct reason.** `abr: mode` is only ever emitted on a
            // probe RESULT, so a recovery gate that never opens produces a log identical to one
            // that was never constructed — and on the host that is exactly what happens
            // (`pipe_auto_original_slow_recover`, 180 s on a 40 Mbit/s link: zero `abr: mode`,
            // Original never re-requested, no way to tell which of four conditions withheld it).
            // Rate-limited to CHANGES so a steady refusal costs one line rather than one a
            // segment; the reason is a name, not a number, so a repeat carries no new information.
            //
            // **A fired probe clears the latch, and that is a fix rather than a nicety.** The
            // reason used to live on the gate as `last_block` and the latch here, two copies of
            // one fact across a module boundary — and they went out of sync exactly when a probe
            // succeeded: the gate reset its copy, this one kept the old reason, and the next
            // refusal FOR THE SAME REASON printed nothing. `probe_due` now returns the reason
            // instead of publishing it, so there is one answer and one channel.
            let permit = match verdict {
                Some(Err(block)) if last_probe_block != Some(block) => {
                    last_probe_block = Some(block);
                    crate::player::log(&format!(
                        "abr: probe withheld reason={} rung={}kbps buf={}ms safe={}kbps",
                        block.as_str(),
                        current_candidate.expected_wire_kbps,
                        sample.buffer.buffered_ms().unwrap_or(-1),
                        delivery.conservative_kbps(),
                    ));
                    None
                }
                Some(Ok(permit)) => {
                    last_probe_block = None;
                    Some(permit)
                }
                _ => None,
            };
            if let Some(permit) = permit {
                if !encoder_cleanup_ready {
                    // Keep discretionary source traffic behind the same exact-cleanup barrier as
                    // HLS exploration. The probe itself reuses the ACTIVE resource and creates no
                    // allocation; this gate only keeps its evidence in a reconciled server epoch.
                    continue;
                }
                let Some(lease) = HlsAutomaticLease::begin(
                    crate::player::HlsAutomaticTransition::Original,
                    expected_user_sequence,
                ) else {
                    crate::player::log(
                        "abr: Original probe deferred behind a playback-clock transition",
                    );
                    continue;
                };
                SHARED
                    .dg_abr_action
                    .store(crate::player::ABR_ACTION_PROBE_ORIGINAL, Ordering::Relaxed);
                SHARED
                    .dg_abr_target_kbps
                    .store(i64::from(control.original_source_kbps()), Ordering::Relaxed);
                // A fresh experiment supersedes the last failure. If this one fails it republishes
                // an exact typed cause below; if it succeeds the stale HTTP code stays gone.
                crate::player::clear_original_failure();
                crate::player::log(
                    "abr: checking finite actual Original range on the live HLS resource",
                );
                let raw_probe = match control.probe_original_while_hls_cancellable(
                    active_encoder,
                    permit.plan,
                    || unsafe { crate::aq::aq_is_aborted(aq) },
                ) {
                    crate::route::OriginalProbeResult::Measured(sample) => sample,
                    crate::route::OriginalProbeResult::Stale => {
                        crate::player::log(
                            "abr: discarded Original probe from a superseded HLS resource",
                        );
                        continue;
                    }
                    crate::route::OriginalProbeResult::Failed { outcome, failure } => {
                        publish_original_probe_failure(failure);
                        let probe_verdict = recovery
                            .as_mut()
                            .map(|gate| gate.observe_probe_failed(&delivery));
                        SHARED.dg_abr_action.store(
                            crate::player::ABR_ACTION_ORIGINAL_PROBE_FAILED,
                            Ordering::Relaxed,
                        );
                        SHARED.dg_abr_target_kbps.store(0, Ordering::Relaxed);
                        crate::player::log(&format!(
                            "abr: Original probe #{} produced no body outcome={}; retaining best HLS \
                             request left={}s verdict={:?}",
                            recovery.as_ref().map(|gate| gate.probes()).unwrap_or(0),
                            outcome.code(),
                            remaining_ms / 1_000,
                            probe_verdict,
                        ));
                        continue;
                    }
                };
                let probe = original_probe_observation(raw_probe);
                let probe_verdict = recovery.as_mut().map(|gate| {
                    gate.observe_probe(
                        probe,
                        current_candidate,
                        &telemetry.production,
                        controller.buffer(),
                        &delivery,
                        remaining_ms,
                    )
                });
                if let Some(cmp) = recovery.as_ref().and_then(|gate| gate.comparison()) {
                    log_original_mode_comparison(cmp);
                }
                let (slow, unc, n, cons, need) = recovery
                    .as_ref()
                    .map(|gate| gate.basis())
                    .unwrap_or_default();
                crate::player::log(&format!(
                    "abr: Original probe #{} measured={}kbps {}KiB/{}ms complete={} left={}s \
                     slow={}kbps unc={}pm n={} cons={}kbps need={}kbps verdict={:?}",
                    recovery.as_ref().map(|gate| gate.probes()).unwrap_or(0),
                    probe.kbps,
                    probe.bytes / 1024,
                    probe.active_us / 1_000,
                    probe.completed as i32,
                    remaining_ms / 1_000,
                    slow,
                    unc,
                    n,
                    cons,
                    need,
                    probe_verdict,
                ));
                if probe_verdict == Some(crate::abr::RecoveryVerdict::Recover) {
                    let measured_kbps = probe.kbps.max(1);
                    // User Pause is an orthogonal accepted event. A probe which was already in
                    // flight may finish and retain evidence, but it may not promote a route while
                    // the viewer owns the clock. Consume a complete hidden pause cycle before
                    // using the result; an active one leaves the verdict latched for Resume.
                    let commit_pause = crate::player::TX.pause_state_sample();
                    if let Some(elapsed) = pause_cursor.consume_completed(commit_pause) {
                        controller
                            .on_resume(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
                    }
                    if commit_pause.paused {
                        *recover_kbps = measured_kbps;
                        crate::player::log(
                            "abr: Original recovery measured during a user hold; retained until Resume",
                        );
                        continue;
                    }
                    match latched_original_recovery(
                        *recover_kbps,
                        Some(measured_kbps),
                        decision,
                        encoder_cleanup_ready,
                        sample.buffer.buffered_ms(),
                        sample.media_duration_ms(),
                    ) {
                        LatchedOriginalRecovery::Switch { kbps, cancel } => {
                            debug_assert!(
                                cancel.is_none(),
                                "a source probe is issued only from a Stay boundary",
                            );
                            match publish_original_recovery(
                                aq,
                                control,
                                active_encoder,
                                kbps,
                                &lease,
                            ) {
                                OriginalRecoveryPublication::Published => {
                                    crate::player::log(&format!(
                                        "abr: source sustainable again at {kbps}kbps; requesting Original on the probe boundary",
                                    ));
                                    break;
                                }
                                OriginalRecoveryPublication::Busy => {
                                    *recover_kbps = measured_kbps;
                                    crate::player::log(
                                        "abr: Original recovery retained behind another route transition",
                                    );
                                }
                                OriginalRecoveryPublication::ClockMoved => {
                                    *recover_kbps = measured_kbps;
                                    crate::player::log(
                                        "abr: Original recovery retained after playback clock moved",
                                    );
                                }
                                OriginalRecoveryPublication::Aborted
                                | OriginalRecoveryPublication::RouteMoved => {
                                    return Err(HlsExit::Aborted);
                                }
                            }
                        }
                        LatchedOriginalRecovery::Wait => {
                            // Conservation currently makes this unreachable for a permitted
                            // probe, but retaining the typed state is safer than dropping a valid
                            // verdict if a future probe contract deliberately changes its runway.
                            *recover_kbps = measured_kbps;
                        }
                    }
                } else {
                    SHARED
                        .dg_abr_action
                        .store(crate::player::ABR_ACTION_STEADY, Ordering::Relaxed);
                    SHARED.dg_abr_target_kbps.store(0, Ordering::Relaxed);
                }
            }
            continue;
        }
        let crate::abr::Decision::Prime(proposal) = decision else {
            continue;
        };
        let reserve_policy = controller
            .pending_reserve_policy(proposal)
            .expect("a Prime decision owns one pending reserve policy");
        if proposal.direction == crate::abr::Direction::Up && !encoder_cleanup_ready {
            reject_hls_abr(
                controller,
                proposal,
                crate::abr::RejectCause::Circumstance,
                now_ms(),
            );
            crate::player::log(
                "abr: quality exploration deferred until PMS confirms prior encoder cleanup",
            );
            continue;
        }
        // The current segment has already been credited to the recovery certificate. Do not put
        // a private quality experiment in front of the main thread's next `Play`: on device that
        // turned a completed 320 kbps recovery into a 4.4 s visible hold while a 22 Mbps warm-up
        // spent its deadline. A downshift remains allowed because it is the recovery operation;
        // an upshift is refused as a circumstance, so it retains no false claim about the rung and
        // is naturally reconsidered after the clock resumes.
        let automatic_kind = match proposal.direction {
            crate::abr::Direction::Up => crate::player::HlsAutomaticTransition::QualityUp,
            crate::abr::Direction::Down => crate::player::HlsAutomaticTransition::QualityDown,
        };
        if !SHARED.hls_automatic_transition_permitted(automatic_kind)
            || !hls_quality_trial_may_start(
                proposal.direction,
                SHARED.hls_rebuffer_requested.load(Ordering::Acquire),
                SHARED.hls_rebuffering.load(Ordering::Acquire),
                paused_now,
            )
        {
            reject_hls_abr(
                controller,
                proposal,
                crate::abr::RejectCause::Circumstance,
                now_ms(),
            );
            crate::player::log(
                "abr: quality transaction deferred until playback clock is running and recovery completes",
            );
            continue;
        }
        // A recovery downshift may legitimately span an internally-held rebuffer clock, so it
        // does not use the upward trial-reserve certificate below. It still owns the same single
        // automatic-actuator slot for the whole PMS/candidate transaction: source recovery and a
        // second rung transaction cannot both pass an earlier snapshot and publish independently.
        let _automatic_lease = if proposal.direction == crate::abr::Direction::Down {
            let Some(lease) = HlsAutomaticLease::begin(automatic_kind, expected_user_sequence)
            else {
                reject_hls_abr(
                    controller,
                    proposal,
                    crate::abr::RejectCause::Circumstance,
                    now_ms(),
                );
                crate::player::log(
                    "abr: recovery transaction superseded before its actuator lease could arm",
                );
                continue;
            };
            Some(lease)
        } else {
            None
        };
        // Upshift is an experiment, not a capacity prediction. Spend only the observable reserve
        // above max(replay boundary, rollback media horizon), and publish the retained replay
        // balance while the transaction is live. One playhead-funded clock follows the initial
        // transaction through its first candidate object. Every blocking control/playlist leg
        // receives a current wall-clock snapshot of that clock, while the media AVIO owns the state
        // itself. A setup-bearing object and its optional ordinary observation share this same
        // clock while both remain staged. A user Pause therefore moves an already-issued snapshot
        // instead of consuming playable reserve. Publish the retained balance BEFORE the control
        // plane: PMS session creation and playlist requests consume it while the native clock
        // advances, even though they move no media bytes.
        let mut _trial_reserve = None;
        let mut exploration_reserve = None;
        if proposal.direction == crate::abr::Direction::Up {
            let (buffer_snapshot, exploration_started_ns) = hls_buffer_snapshot_with_playhead(None);
            let Some(buffered_ms) = buffer_snapshot.buffered_ms() else {
                reject_hls_abr(
                    controller,
                    proposal,
                    crate::abr::RejectCause::Circumstance,
                    now_ms(),
                );
                crate::player::log(
                    "abr: candidate reserve unreadable before exploration; staying on current rung",
                );
                continue;
            };
            let Some(runway_ms) = controller.prime_runway_ms() else {
                reject_hls_abr(
                    controller,
                    proposal,
                    crate::abr::RejectCause::Circumstance,
                    now_ms(),
                );
                crate::player::log(
                    "abr: candidate has no completed current acquisition to preserve; staying",
                );
                continue;
            };
            let Some(budget_ms) = controller.exploration_budget_ms(buffered_ms) else {
                reject_hls_abr(
                    controller,
                    proposal,
                    crate::abr::RejectCause::Circumstance,
                    now_ms(),
                );
                crate::player::log(
                    "abr: candidate has no reserve above rollback boundary; staying on current rung",
                );
                continue;
            };
            // The proposal captured an earlier reserve. This re-read is the budget that is
            // actually armed end to end, so it is the only value a failed experiment may retain
            // in its per-rung certificate.
            if !controller.set_executed_exploration_budget(proposal, budget_ms) {
                crate::player::log(&format!(
                    "abr: actual exploration budget {}ms no longer releases {}kbps; staying on current rung",
                    budget_ms,
                    proposal.rung.kbps(),
                ));
                continue;
            }
            let Some(trial_reserve) = HlsTrialReserve::arm(runway_ms, expected_user_sequence)
            else {
                reject_hls_abr(
                    controller,
                    proposal,
                    crate::abr::RejectCause::Circumstance,
                    now_ms(),
                );
                crate::player::log(
                    "abr: clock transition superseded exploration before its trial could arm",
                );
                continue;
            };
            _trial_reserve = Some(trial_reserve);
            exploration_reserve = Some(ReserveDeadlineState::from_playhead_at(
                exploration_started_ns,
                std::time::Duration::from_millis(u64::try_from(budget_ms).unwrap_or(u64::MAX)),
                false,
            ));
            crate::player::log(&format!(
                "abr: exploration target={}kbps buf={}ms runway={}ms budget={}ms",
                proposal.rung.kbps(),
                buffered_ms,
                runway_ms,
                budget_ms,
            ));
        }
        // Records the whole transaction on every exit path, including the twelve rejects. Drop
        // emits it; nothing below has to remember to.
        let refreshing_same_request = proposal.rung == controller.current();
        let mut tx = TxTrace::open(
            proposal,
            controller.current(),
            sample,
            &controller.delivery(),
        );
        publish_hls_abr_action(proposal, refreshing_same_request, None);
        let offset_ns = SHARED
            .disp_base
            .load(Ordering::Relaxed)
            .max(0)
            .checked_add(timeline.end_ns())
            .ok_or(HlsExit::Failed("HLS candidate content offset overflow"))?;
        let offset_micros = offset_ns / 1_000;
        let prime_deadline = exploration_snapshot(&mut exploration_reserve);
        let primed = match control.prime(active_encoder, proposal, offset_micros, prime_deadline) {
            Ok(primed) => primed,
            Err(refusal) => {
                if unsafe { crate::aq::aq_is_aborted(aq) } {
                    tx.finish("aborted");
                    return Err(HlsExit::Aborted);
                }
                // **The cause is the SERVER's answer, not a guess at this call site.** `prime`
                // has four exits and only one — a PMS refusal of this rung's ceiling — says
                // anything about the candidate. Reading a bare failure as `Candidate` charged the
                // other three a full `E_tx` refill debt (up to ~4x `E_tx` of blocked climbing) for
                // an encoder that moved underneath, a missing client or an unresolved
                // control-plane result. None is ordinal evidence about the requested rung.
                let (cause, why) = match refusal {
                    crate::route::PrimeRefusal::Rung => {
                        (crate::abr::RejectCause::Structural, "prime_refused")
                    }
                    crate::route::PrimeRefusal::Deadline
                        if !exploration_timeout_is_final(
                            &mut exploration_reserve,
                            prime_deadline,
                        ) =>
                    {
                        // A decision request is not safe to replay blindly: PMS may have
                        // registered it before the old snapshot fired. `prime` has already
                        // queued exact cleanup, so retire this attempt without teaching the
                        // frontier that the rung exhausted a reserve the playhead did not
                        // spend. Resume can propose it again from fresh completed evidence.
                        (crate::abr::RejectCause::Circumstance, "control_clock_moved")
                    }
                    crate::route::PrimeRefusal::Deadline => {
                        (crate::abr::RejectCause::Censored, "control_deadline")
                    }
                    crate::route::PrimeRefusal::Control => {
                        (crate::abr::RejectCause::Circumstance, "control_unresolved")
                    }
                    crate::route::PrimeRefusal::Session => {
                        (crate::abr::RejectCause::Circumstance, "prime_session_moved")
                    }
                };
                reject_hls_abr_after_transaction(controller, proposal, cause, &mut tx, now_ms());
                tx.finish(why);
                crate::player::log("abr: candidate registration rejected; staying on current rung");
                continue;
            }
        };
        tx.mark_prime();
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            control.abandon(&primed.encoder_session);
            tx.finish("aborted");
            return Err(HlsExit::Aborted);
        }
        if exploration_expired(&mut exploration_reserve) {
            control.abandon(&primed.encoder_session);
            reject_hls_abr_after_transaction(
                controller,
                proposal,
                crate::abr::RejectCause::Censored,
                &mut tx,
                now_ms(),
            );
            tx.finish("control_deadline");
            crate::player::log("abr: candidate spent its exploration reserve in the control plane");
            continue;
        }
        let candidate_url = crate::plex::StreamUrl::parse(&primed.url);
        if candidate_url.origin != *origin {
            control.abandon(&primed.encoder_session);
            reject_hls_abr_after_transaction(
                controller,
                proposal,
                crate::abr::RejectCause::Circumstance,
                &mut tx,
                now_ms(),
            );
            tx.finish("origin_changed");
            crate::player::log("abr: candidate changed origin; rejected");
            continue;
        }
        let mut candidate = match hls_cursor_open(
            &candidate_url.origin,
            &candidate_url.path,
            aq,
            hs,
            false,
            exploration_reserve.as_mut(),
        ) {
            Ok(candidate) => candidate,
            Err(HlsExit::Aborted) => {
                control.abandon(&primed.encoder_session);
                tx.finish("aborted");
                return Err(HlsExit::Aborted);
            }
            Err(error) => {
                control.abandon(&primed.encoder_session);
                reject_hls_abr_after_transaction(
                    controller,
                    proposal,
                    abr_reject_for_hls_exit(error),
                    &mut tx,
                    now_ms(),
                );
                tx.finish(if matches!(error, HlsExit::PrimeExpired) {
                    "master_deadline"
                } else {
                    "no_master_playlist"
                });
                crate::player::log("abr: candidate master failed; staying on current rung");
                continue 'segments;
            }
        };
        tx.mark_master(candidate.declared_bps);
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            control.abandon(&primed.encoder_session);
            tx.finish("aborted");
            return Err(HlsExit::Aborted);
        }
        if exploration_expired(&mut exploration_reserve) {
            control.abandon(&primed.encoder_session);
            reject_hls_abr_after_transaction(
                controller,
                proposal,
                crate::abr::RejectCause::Censored,
                &mut tx,
                now_ms(),
            );
            tx.finish("master_deadline");
            crate::player::log("abr: candidate spent its exploration reserve before media");
            continue;
        }
        let candidate_segment =
            match hls_cursor_next(&mut candidate, aq, hs, exploration_reserve.as_mut()) {
                Ok(Some(segment)) => segment,
                Err(HlsExit::Aborted) => {
                    control.abandon(&primed.encoder_session);
                    tx.finish("aborted");
                    return Err(HlsExit::Aborted);
                }
                Err(error) => {
                    control.abandon(&primed.encoder_session);
                    reject_hls_abr_after_transaction(
                        controller,
                        proposal,
                        abr_reject_for_hls_exit(error),
                        &mut tx,
                        now_ms(),
                    );
                    tx.finish(if matches!(error, HlsExit::PrimeExpired) {
                        "playlist_deadline"
                    } else {
                        "no_media_playlist"
                    });
                    crate::player::log(
                        "abr: candidate produced no segment; staying on current rung",
                    );
                    continue 'segments;
                }
                Ok(None) => {
                    control.abandon(&primed.encoder_session);
                    reject_hls_abr_after_transaction(
                        controller,
                        proposal,
                        crate::abr::RejectCause::Circumstance,
                        &mut tx,
                        now_ms(),
                    );
                    tx.finish("candidate_ended");
                    crate::player::log("abr: candidate stream ended before media; staying");
                    continue 'segments;
                }
            };
        // The whole control plane shares one playhead-funded exploration reserve. Each blocking
        // layer receives a fresh absolute snapshot, so a timeout retains the budget actually
        // executed while a Pause or native hold moves the boundary instead of spending it.
        tx.mark_control_plane();
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            control.abandon(&primed.encoder_session);
            tx.finish("aborted");
            return Err(HlsExit::Aborted);
        }
        if exploration_expired(&mut exploration_reserve) {
            control.abandon(&primed.encoder_session);
            reject_hls_abr_after_transaction(
                controller,
                proposal,
                crate::abr::RejectCause::Censored,
                &mut tx,
                now_ms(),
            );
            tx.finish("playlist_deadline");
            crate::player::log("abr: candidate spent its exploration reserve before media");
            continue;
        }
        // The remaining candidate media budget is whatever is left of the same end-to-end
        // exploration surplus after session creation and both playlists. A remote PMS may spend a
        // material fraction of it registering the encoder; that time consumes the viewer's
        // reserve even though it transfers no media, so excluding it would over-grant the trial.
        //
        // **The buffer is read HERE for recovery.** Upward exploration instead carries the same
        // fixed clock it armed before the control plane, whose remaining value already subtracts
        // exactly Δplayhead. Reading a fresh buffer as a new exploration grant would let segments
        // arriving on some future concurrent implementation silently enlarge `E` mid-transaction.
        let (reserve_snapshot, reserve_started_ns) = hls_buffer_snapshot_with_playhead(None);
        let Some(reserve_ms) = reserve_snapshot.buffered_ms() else {
            // No readable reserve means nothing can be said about what this transaction can
            // afford, and a transaction with no affordability bound is the unbounded case being
            // removed. **Refusing costs nothing**, which is what `RejectCause::Circumstance` on the
            // next line exists to say: the controller cannot have proposed on an unknown reserve in
            // the first place, so the lane fell silent between the proposal and here — a seek —
            // and that says nothing about the RUNG. No backoff is armed and the next sample
            // resolves it. (This read "sets a one-sample cooldown" until I6, describing a mechanism
            // that never blocked a segment even before it was deleted: the decrement ran before
            // the check, so `K = 1` was a no-op.)
            control.abandon(&primed.encoder_session);
            reject_hls_abr_after_transaction(
                controller,
                proposal,
                crate::abr::RejectCause::Circumstance,
                &mut tx,
                now_ms(),
            );
            tx.finish("reserve_unreadable");
            crate::player::log(
                "abr: candidate reserve unreadable; cannot bound the transfer, staying on current rung",
            );
            continue;
        };
        let reserve = exploration_reserve
            .as_ref()
            .and_then(ReserveDeadlineState::remaining)
            .unwrap_or_else(|| crate::abr::reserve_as_budget(reserve_ms));
        // The floor is a DOWNSHIFT's only protection against the reserve it is trying to refill:
        // see `candidate_warmup_budget`. Both terms are read here, at the transaction, so the
        // prediction describes the rung actually being primed against the capacity actually
        // measured — `expected_wire_kbps` is the catalog's observed output for the target, not its
        // request ceiling, and `conservative_kbps` comes only from completed segments.
        let delivery = controller.delivery();
        let predicted = crate::abr::predicted_transfer(
            controller
                .catalog()
                .candidate(proposal.rung)
                .expected_wire_kbps,
            candidate_segment.duration,
            delivery.conservative_kbps(),
            delivery.uncertainty_pm,
        );
        let warmup_budget = crate::abr::candidate_warmup_budget(
            proposal,
            candidate_segment.duration,
            reserve,
            predicted,
            // What a segment on this playback costs BEFORE any of its bytes move. `predicted` is
            // `bits / rate` and pays for the body read only; a warm-up also has to open, request
            // and probe, and on a fresh candidate session those are dearer than the steady-state
            // figure this reports.
            std::time::Duration::from_micros(controller.worst_overhead_us()),
        );
        let media_budget = crate::abr::candidate_media_reserve_deadline(
            proposal,
            reserve_policy,
            SHARED.hls_rebuffering.load(Ordering::Acquire),
            warmup_budget,
        );
        if let Some(media_budget) = media_budget {
            tx.mark_warmup_deadline(media_budget);
        } else {
            crate::player::log(
                "abr: terminal floor recovery keeps the only candidate response alive",
            );
        }
        let deadline_yields_to_rebuffer =
            proposal.direction == crate::abr::Direction::Down && proposal.rung.at_floor();
        let candidate_deadline = candidate_reserve_deadline(
            exploration_reserve,
            media_budget,
            deadline_yields_to_rebuffer,
            reserve_started_ns,
            proposal.direction,
        );
        let mut staged_timeline = timeline;
        let mut candidate_clock = match staged_timeline.begin(candidate_segment.duration) {
            Ok(clock) => clock,
            Err(_) => {
                control.abandon(&primed.encoder_session);
                reject_hls_abr_after_transaction(
                    controller,
                    proposal,
                    crate::abr::RejectCause::Circumstance,
                    &mut tx,
                    now_ms(),
                );
                tx.finish("timeline_overflow");
                return Err(HlsExit::Failed("HLS candidate timeline overflow"));
            }
        };
        let candidate_output = match unsafe {
            hls_demux_segment(
                &candidate_segment,
                &candidate.auth,
                &mut candidate_clock,
                aq,
                hs,
                acodec,
                candidate_deadline,
                // Candidate responses never carry a prefix `StallGuard`. PMS may pause a sized
                // response while its JIT encoder catches up, so a prefix-rate extrapolation is
                // not proof that the unseen remainder will miss `candidate_deadline` above. When
                // that reserve clock exists, each `read_until` gets its current wall projection and
                // the callback classifies the wake against the owning playhead clock. Terminal-floor
                // recovery has no reserve deadline because the robust rollback guarantee is already
                // absent (even if a positive remnant remains) and no cheaper response exists.
                None,
            )
        } {
            Ok(output) => output,
            Err(HlsExit::Aborted) => {
                control.abandon(&primed.encoder_session);
                tx.finish("aborted");
                return Err(HlsExit::Aborted);
            }
            Err(HlsExit::PrimeExpired) => {
                control.abandon(&primed.encoder_session);
                reject_hls_abr_after_transaction(
                    controller,
                    proposal,
                    crate::abr::RejectCause::Censored,
                    &mut tx,
                    now_ms(),
                );
                tx.finish("warmup_deadline");
                crate::player::log(&format!(
                    "abr: {:?} candidate warm-up exceeded deadline; staying on current rung",
                    proposal.direction,
                ));
                continue;
            }
            Err(error) => {
                control.abandon(&primed.encoder_session);
                reject_hls_abr_after_transaction(
                    controller,
                    proposal,
                    abr_reject_for_hls_exit(error),
                    &mut tx,
                    now_ms(),
                );
                tx.finish("warmup_failed");
                crate::player::log("abr: candidate segment failed; staying on current rung");
                continue;
            }
        };
        tx.mark_media(&candidate_output, false);
        staged_timeline.commit(candidate_clock);
        let mut candidate_outputs = vec![(
            candidate_output,
            candidate_clock,
            candidate_segment.duration,
        )];
        let mut candidate_transition: Option<HlsCandidateTransition> = None;

        // The boundary object is still direct evidence whenever it satisfies `A<=D && B_post>=A`:
        // commit it immediately. An `A>D` boundary contains one-time PMS/session/JIT setup and may
        // need one ordinary object to identify repeatable cadence. Both objects remain STAGED:
        // the original exploration clock must fund the whole observation without media credit.
        // Publishing the setup object first and later rejecting the candidate mixed two encoder
        // owners on one decoder timeline, forced cursor-by-count repair, and produced the observed
        // raster/A-V excursion. A candidate now has exactly two outcomes: commit then feed every
        // staged object, or discard all of them while the old cursor remains untouched.
        let first_raster_ready = hls_raster_within(
            candidate_outputs[0].0.video_width,
            candidate_outputs[0].0.video_height,
            proposal.rung,
        );
        let first_quality_improves = proposal.direction != crate::abr::Direction::Up
            || hls_upshift_strictly_improves(
                output.video_width,
                output.video_height,
                cursor.declared_bps,
                candidate_outputs[0].0.video_width,
                candidate_outputs[0].0.video_height,
                candidate.declared_bps,
            );
        let first_sample = hls_segment_sample(&candidate_outputs[0].0, candidate_outputs[0].2);
        let mut verdict = first_sample.map_or(crate::abr::CandidateVerdict::Incomplete, |sample| {
            if proposal.direction == crate::abr::Direction::Up {
                controller.candidate_boundary_verdict(proposal, sample, candidate.declared_bps)
            } else {
                controller.candidate_verdict(proposal, sample, candidate.declared_bps)
            }
        });

        if first_raster_ready
            && first_quality_improves
            && verdict == crate::abr::CandidateVerdict::SetupBearing
        {
            if exploration_reserve.is_none() || exploration_expired(&mut exploration_reserve) {
                control.abandon(&primed.encoder_session);
                reject_hls_abr_after_transaction(
                    controller,
                    proposal,
                    crate::abr::RejectCause::Censored,
                    &mut tx,
                    now_ms(),
                );
                tx.finish("repeatable_unfunded");
                crate::player::log(
                    "abr: setup-bearing candidate exhausted its original exploration budget",
                );
                continue;
            }
            crate::player::log(&format!(
                "abr: setup-bearing candidate retains one staged observation budget={}ms",
                exploration_reserve
                    .as_ref()
                    .and_then(ReserveDeadlineState::remaining)
                    .map_or(0, |left| left.as_millis()),
            ));
            let repeatable_segment =
                match hls_cursor_next(&mut candidate, aq, hs, exploration_reserve.as_mut()) {
                    Ok(Some(segment)) => segment,
                    Ok(None) => {
                        control.abandon(&primed.encoder_session);
                        reject_hls_abr_after_transaction(
                            controller,
                            proposal,
                            crate::abr::RejectCause::Circumstance,
                            &mut tx,
                            now_ms(),
                        );
                        tx.finish("candidate_ended_after_setup");
                        continue;
                    }
                    Err(HlsExit::Aborted) => {
                        control.abandon(&primed.encoder_session);
                        tx.finish("aborted");
                        return Err(HlsExit::Aborted);
                    }
                    Err(error) => {
                        control.abandon(&primed.encoder_session);
                        reject_hls_abr_after_transaction(
                            controller,
                            proposal,
                            abr_reject_for_hls_exit(error),
                            &mut tx,
                            now_ms(),
                        );
                        tx.finish(if matches!(error, HlsExit::PrimeExpired) {
                            "repeatable_playlist_deadline"
                        } else {
                            "repeatable_playlist_failed"
                        });
                        continue;
                    }
                };
            let mut repeatable_clock = match staged_timeline.begin(repeatable_segment.duration) {
                Ok(clock) => clock,
                Err(_) => {
                    control.abandon(&primed.encoder_session);
                    reject_hls_abr_after_transaction(
                        controller,
                        proposal,
                        crate::abr::RejectCause::Circumstance,
                        &mut tx,
                        now_ms(),
                    );
                    tx.finish("repeatable_timeline_overflow");
                    return Err(HlsExit::Failed("HLS candidate timeline overflow"));
                }
            };
            let repeatable_deadline = exploration_reserve
                .expect("an upward setup-bearing observation owns its original reserve clock");
            let repeatable_output = match unsafe {
                hls_demux_segment(
                    &repeatable_segment,
                    &candidate.auth,
                    &mut repeatable_clock,
                    aq,
                    hs,
                    acodec,
                    repeatable_deadline,
                    None,
                )
            } {
                Ok(output) => output,
                Err(HlsExit::Aborted) => {
                    control.abandon(&primed.encoder_session);
                    tx.finish("aborted");
                    return Err(HlsExit::Aborted);
                }
                Err(error) => {
                    control.abandon(&primed.encoder_session);
                    reject_hls_abr_after_transaction(
                        controller,
                        proposal,
                        abr_reject_for_hls_exit(error),
                        &mut tx,
                        now_ms(),
                    );
                    tx.finish(if matches!(error, HlsExit::PrimeExpired) {
                        "repeatable_deadline"
                    } else {
                        "repeatable_failed"
                    });
                    crate::player::log(
                        "abr: candidate produced no complete repeatable object; staying current",
                    );
                    continue;
                }
            };
            tx.mark_media(&repeatable_output, true);
            staged_timeline.commit(repeatable_clock);
            candidate_outputs.push((
                repeatable_output,
                repeatable_clock,
                repeatable_segment.duration,
            ));
            verdict = hls_segment_sample(&candidate_outputs[1].0, candidate_outputs[1].2)
                .map_or(crate::abr::CandidateVerdict::Incomplete, |sample| {
                    controller.candidate_verdict(proposal, sample, candidate.declared_bps)
                });
        }

        let raster_ready = !hls_candidate_requires_rung_box(proposal, reserve_policy)
            || candidate_outputs.iter().all(|(output, _, _)| {
                hls_raster_within(output.video_width, output.video_height, proposal.rung)
            });
        let (candidate_output, _, candidate_duration) =
            candidate_outputs.last().expect("candidate output");
        let quality_improves = proposal.direction != crate::abr::Direction::Up
            || hls_upshift_strictly_improves(
                output.video_width,
                output.video_height,
                cursor.declared_bps,
                candidate_output.video_width,
                candidate_output.video_height,
                candidate.declared_bps,
            );
        tx.mark_candidate_evidence(candidate_output, *candidate_duration);
        let candidate_sample = hls_segment_sample(candidate_output, *candidate_duration);
        let disposition = hls_candidate_disposition(raster_ready, quality_improves, verdict);
        if let HlsCandidateDisposition::Discard { cause, outcome } = disposition {
            control.abandon(&primed.encoder_session);
            reject_hls_abr_after_transaction(controller, proposal, cause, &mut tx, now_ms());
            if !raster_ready {
                crate::player::log(&format!(
                    "abr: candidate raster {}x{} exceeds {}x{}; staying on current rung",
                    candidate_output.video_width,
                    candidate_output.video_height,
                    proposal.rung.raster().0,
                    proposal.rung.raster().1,
                ));
            } else if !quality_improves {
                crate::player::log(&format!(
                    "abr: candidate request {}kbps produced {}x{} @ {}kbps versus current {}x{} @ \
                     {}kbps; no measured quality gain, staying on current rung",
                    proposal.rung.kbps(),
                    candidate_output.video_width,
                    candidate_output.video_height,
                    candidate.declared_bps / 1_000,
                    output.video_width,
                    output.video_height,
                    cursor.declared_bps / 1_000,
                ));
            } else {
                crate::player::log(&format!(
                    "abr: candidate verdict={verdict:?}; discarded {} staged segment(s) without \
                     changing the active media owner",
                    candidate_outputs.len(),
                ));
            }
            tx.finish(outcome);
            continue;
        }
        let sample = candidate_sample.expect("a ready candidate has a completed segment sample");
        let Some(candidate_variant) = crate::abr::ObservedHlsVariant::new(
            candidate.declared_bps,
            candidate_output.video_width,
            candidate_output.video_height,
        ) else {
            control.abandon(&primed.encoder_session);
            return Err(HlsExit::Failed(
                "ABR candidate committed invalid output geometry",
            ));
        };
        // Validation ends here. Freeze the decision timestamp before the feed loop, which can
        // block on `aq_push` against a full queue; `feed=` records that backpressure separately.
        // The route must remain old until every candidate AU which precedes the switch is safely
        // queued. Otherwise teardown can abort a blocked feed after publication and leave a live
        // route naming media the playback pipeline never received.
        tx.mark_decided();
        if candidate_transition.is_none() {
            let automatic_token = _trial_reserve
                .as_ref()
                .map(HlsTrialReserve::token)
                .or_else(|| _automatic_lease.as_ref().map(HlsAutomaticLease::token))
                .expect("every HLS candidate transaction owns one automatic actuator lease");
            candidate_transition = match HlsCandidateTransition::arm(automatic_token) {
                Ok(transition) => Some(transition),
                Err(error) => {
                    control.abandon(&primed.encoder_session);
                    tx.finish("publication_overlap");
                    return Err(error);
                }
            };
        }
        let feed_started = std::time::Instant::now();
        for (output, _, _) in &candidate_outputs {
            if let Err(error) = hls_feed_segment(output, aq, aqa) {
                control.abandon(&primed.encoder_session);
                tx.finish(if matches!(error, HlsExit::Aborted) {
                    "aborted"
                } else {
                    "commit_feed_failed"
                });
                return Err(error);
            }
        }
        tx.mark_feed(feed_started);
        timeline = staged_timeline;

        // The queue abort is teardown's actual linearization point. Publish both copies of the
        // live encoder while holding that SAME mutex, so exactly one side wins: either teardown
        // prevents the route change, or publication captures the old encoder before teardown can
        // join this worker. No cleanup/network work is allowed inside the closure.
        let publication = publish_hls_candidate(
            aq,
            control,
            controller,
            active_encoder,
            &primed,
            proposal,
            sample,
            candidate_variant,
            now_ms(),
        );
        let previous = match publication {
            CandidatePublication::Aborted => {
                control.abandon(&primed.encoder_session);
                tx.finish("aborted");
                return Err(HlsExit::Aborted);
            }
            CandidatePublication::SessionEnded => {
                control.abandon(&primed.encoder_session);
                tx.finish("commit_session_ended");
                return Err(HlsExit::Aborted);
            }
            CandidatePublication::RouteMoved => {
                // The route changed independently while this candidate was in flight. The
                // callback never ran, so controller and worker-local encoder also remain old.
                // The transaction still owns the candidate cleanup.
                control.abandon(&primed.encoder_session);
                tx.finish("commit_route_moved");
                return Err(HlsExit::Aborted);
            }
            CandidatePublication::ControllerRejected => {
                control.abandon(&primed.encoder_session);
                tx.finish("commit_controller_rejected");
                return Err(HlsExit::Failed(
                    "ABR candidate commit lost its validated evidence",
                ));
            }
            CandidatePublication::Active { previous } => previous,
        };
        // The committed candidate is already the first completed acquisition at the new
        // operating point. Publish its exact replay boundary before the trial lifetime marker and
        // explicit candidate-generation settlement. Leaving the old rung's value here would authorize
        // a restart against evidence from an actuator that no longer owns the media stream. During
        // an internal hold, settling the fence starts a fresh recovery epoch immediately before its
        // release store; the next ordinary active segment, not this setup-bearing candidate object,
        // certifies resume.
        if let Some(runway_ms) = controller.prime_runway_ms() {
            SHARED
                .hls_prime_runway_ms
                .store(runway_ms, Ordering::Release);
        }
        // Only now is `outcome=committed` an exact certificate: the staged media was fed and the
        // controller has atomically moved the rung, reset the old bag and seeded the new one.
        tx.finish("committed");
        SHARED
            .dg_abr_kbps
            .store(i64::from(proposal.rung.kbps()), Ordering::Relaxed);
        SHARED.dg_abr_declared_kbps.store(
            i64::try_from(candidate.declared_bps / 1_000).unwrap_or(i64::MAX),
            Ordering::Relaxed,
        );
        publish_hls_abr_action(proposal, refreshing_same_request, Some(true));
        crate::player::report::note_hls_committed_for(
            control.trace_generation(),
            proposal.direction,
            proposal.rung,
            refreshing_same_request,
        );
        // Promoted: from here this cursor IS the playback, so its playlist is the one that may
        // speak for the film's duration.
        candidate.publishes_duration = true;
        cursor = candidate;
        // Any terminal source-mode verdict was computed against the previous HLS operating point.
        // Invalidate it explicitly: an Up keeps the completed source evidence for re-scoring on the
        // next ordinary segment, while a safety Down retires evidence from the failed service
        // regime and returns the source gate to a fresh bounded experiment. Non-terminal probe
        // states remain unchanged, and no stale source switch stays actionable.
        if let Some(gate) = recovery.as_mut() {
            gate.on_hls_commit(proposal.direction);
        }
        *recover_kbps = 0;
        last_probe_block = None;

        // All behavioral projections now name the candidate: controller, global route,
        // worker-local encoder/cursor, runway and recovery policy. Release both main-thread gates
        // before encoder cleanup, whose task-spawn fallback may perform synchronous network I/O.
        settle_candidate_transition(&mut candidate_transition);
        drop(_trial_reserve.take());
        control.retire(previous);
        // **`out=` is what was DECODED; the pair before it is the rung's bounding box.** They are
        // routinely different and the difference is the point: M3 (2026-08-28) measured PMS
        // producing 1280x720 for BOTH `P720` and `P1080M6` against a 4K source, and 1918x802 for
        // every rung from `P1080M6` up against a 1080p one. So a commit that crosses a catalog
        // raster band is frequently invisible to a viewer, and `run.py`'s `raster_changes` — the
        // plan's device co-grader — counted those catalog bands because this line printed nothing
        // else. `abr.rs` states the rule this violated: *a rung's raster is a BOUNDING BOX, not a
        // target*, and reading one as the other has already shipped as a device bug once.
        //
        // APPENDED rather than substituted: `RE_ABR_UP` and `RE_ABR_COMMIT` both parse the
        // leading form, so replacing it in place would silently retire two graders. Old logs stay
        // readable and `abr_raster_changes` prefers `out=` when it is there.
        let observed = candidate_outputs
            .last()
            .map(|(output, _, _)| (output.video_width, output.video_height))
            .unwrap_or((0, 0));
        crate::player::log(&format!(
            "abr: committed {:?} to {}kbps {}x{} out={}x{}",
            proposal.direction,
            proposal.rung.kbps(),
            proposal.rung.raster().0,
            proposal.rung.raster().1,
            observed.0,
            observed.1,
        ));
    }
    Ok(())
}

/// The demux thread body (spawned by `engine::start_bufferfeed`).
///
/// Takes an [`Origin`](crate::plex::Origin) rather than a `(host, port)` pair because **the scheme
/// decides the transport**: `http` reads through the Engine's `stream.rs` socket, `https` through
/// [`crate::curlio`]. An origin is parsed from a URL and never rebuilt from an address, which is
/// what keeps the `plex.direct` hostname TLS validates against intact all the way down here
/// (`plex/origin.rs`). `hs` is still passed on both paths — it is the Engine's, and it stays
/// unused (fd = -1, published as `SHARED.hs_ptr`) when the origin turns out to be https.
pub(crate) fn demux(
    origin: crate::plex::Origin,
    path: String,
    acodec: String,
    abr: Option<(crate::route::HlsAbrControl, crate::route::WorkerTicket)>,
    auto_original: Option<crate::route::AutoOriginalWatch>,
    aq: SendPtr<AuQueue>,
    aqa: SendPtr<AuQueue>,
    hs: SendPtr<HttpStream>,
) {
    PUSHED_ANY.store(false, Ordering::Relaxed);
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
    if path
        .split_once('?')
        .map_or(path.as_str(), |(plain, _)| plain)
        .to_ascii_lowercase()
        .ends_with(".m3u8")
    {
        crate::player::log("hls: segmented demux start");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            hls_demux(&origin, &path, &acodec, abr, aq_p, aqa_p, hs_p)
        }));
        match outcome {
            Ok(Ok(())) | Ok(Err(HlsExit::Aborted)) => {}
            Ok(Err(HlsExit::Failed(why))) => {
                crate::player::log(&format!("hls: demux failed: {why}"));
                if PUSHED_ANY.load(Ordering::Relaxed) {
                    SHARED.demux_io_failed.store(true, Ordering::Release);
                } else {
                    SHARED.demux_failed.store(true, Ordering::Release);
                }
            }
            Ok(Err(HlsExit::NotReady)) => {
                crate::player::log("hls: demux failed: playlist resource was not ready");
                if PUSHED_ANY.load(Ordering::Relaxed) {
                    SHARED.demux_io_failed.store(true, Ordering::Release);
                } else {
                    SHARED.demux_failed.store(true, Ordering::Release);
                }
            }
            Ok(Err(HlsExit::PrimeExpired)) => {
                crate::player::log("hls: demux failed: unexpected active-stream prime deadline");
                SHARED.demux_io_failed.store(true, Ordering::Release);
            }
            Ok(Err(HlsExit::StallAbort(_))) => {
                // The abort rule is handled where it fires, inside the segment loop; reaching here
                // means it escaped one of the candidate paths, which do not arm it.
                crate::player::log("hls: demux failed: stall abort escaped the segment loop");
                SHARED.demux_io_failed.store(true, Ordering::Release);
            }
            Err(_) => {
                crate::player::log("hls: demux panicked");
                SHARED.demux_failed.store(true, Ordering::Release);
            }
        }
        if !PUSHED_ANY.load(Ordering::Relaxed) && !unsafe { crate::aq::aq_is_aborted(aq_p) } {
            SHARED.demux_failed.store(true, Ordering::Release);
        }
        crate::aq::aq_set_eof(aq_p);
        crate::aq::aq_set_eof(aqa_p);
        crate::player::log("hls: segmented demux ended");
        return;
    }
    if let Some(watch) = auto_original.as_ref() {
        // Auto deliberately begins by trying Original. Publish that state before the first
        // measurement window completes so Stats for Nerds says what the policy is doing instead
        // of looking inactive during the exact startup interval a user is trying to diagnose.
        SHARED
            .dg_abr_mode
            .store(crate::player::ABR_MODE_ORIGINAL, Ordering::Relaxed);
        SHARED
            .dg_abr_kbps
            .store(i64::from(watch.source_kbps), Ordering::Relaxed);
        SHARED.dg_abr_net_kbps.store(-1, Ordering::Relaxed);
        SHARED.dg_abr_buffer_ms.store(-1, Ordering::Relaxed);
        SHARED.dg_abr_ratio_pm.store(-1, Ordering::Relaxed);
        SHARED
            .dg_abr_action
            .store(crate::player::ABR_ACTION_STEADY, Ordering::Relaxed);
        SHARED.dg_abr_target_kbps.store(0, Ordering::Relaxed);
        SHARED.dg_abr_unsafe_deficit_ms.store(0, Ordering::Relaxed);
    }
    let make_original_watch = |watch: &crate::route::AutoOriginalWatch| {
        crate::abr::OriginalModeController::new(
            watch.source_kbps,
            crate::abr::AbrPolicy::measured(),
            watch.catalog,
            watch.history,
            watch.features,
        )
    };
    let mut original_context = auto_original;
    let mut original_watch = original_context.as_ref().and_then(make_original_watch);
    // Same accepted-event cursor as HLS. Progressive reads and queue pushes can hide an entire
    // pause cycle, so a local `paused_since` sampled around packets is not a valid clock.
    let mut original_pause_cursor =
        crate::player::UserPauseCursor::new(crate::player::TX.pause_state_sample());
    // **The Original watchdog's WALL clock** (N13). Its persistence rule used to count 750 ms
    // windows of ACTIVE BODY-READ time, which under backpressure — the healthy full-buffer case —
    // spans unbounded wall clock, so "six windows" was not a duration at all. One `Instant` for the
    // whole progressive session, read absolutely, for the reason `advance_to` documents.
    let mut original_since = std::time::Instant::now();
    let port = origin.port() as c_int;
    // `host()` is the origin's BARE host — a v6 literal arrives unbracketed, which is what
    // `stream.rs` wants; `base()` re-brackets it, which is what a URL needs. Both spellings come
    // from the one `Origin` rather than being reconstructed, which is the whole point of the type.
    let host_c = CString::new(origin.host().to_owned()).unwrap_or_default();
    let url = format!("{}{}", origin.base(), path); // https only — carries the token, never logged
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
                // Publish curl's wake target BEFORE the final AU-abort check. These two operations
                // close each other's race: teardown either sees and signals the reservation, or it
                // aborts the lane first and this check refuses to start I/O. Creating/registering the
                // source only after the check leaves a window where teardown's one wake sees nothing
                // and the demuxer then opens a fresh connection under the main thread's join.
                let mut curl_open = if origin.is_tls() {
                    match crate::curlio::CurlSource::reserve_open() {
                        Ok(r) => Some(r),
                        Err(e) => {
                            crate::player::log(&format!("ff: https reservation FAILED: {e:?}"));
                            SHARED.demux_failed.store(true, Ordering::Release);
                            break;
                        }
                    }
                } else {
                    None
                };
                // Teardown may have raced us here (it aborts the lanes, then signals both transports,
                // then JOINS this thread on the main thread). The reservation above makes the HTTPS
                // side as race-free as the socket's already-published `hs_ptr`.
                if crate::aq::aq_is_aborted(aq_p) {
                    crate::player::log("ff: aborted before reopen");
                    break;
                }
                // ONE decision, here, from the scheme — and the only place in the media path that
                // makes it. Both arms publish the same two diagnostics (`dg_http_status`, `file_size`)
                // before anything else can fail, because the read-out panel is the first thing anybody
                // looks at when a part will not play and it must mean the same thing either way.
                let (src, size) = if origin.is_tls() {
                    let reservation = curl_open
                        .take()
                        .expect("TLS reserved its abort handle above");
                    match crate::curlio::CurlSource::open_reserved(&url, 0, reservation) {
                        Ok(cs) => {
                            let (st, size) = (cs.status(), cs.size());
                            SHARED.file_size.store(size, Ordering::Release);
                            SHARED.dg_http_status.store(st, Ordering::Relaxed);
                            crate::player::log(&format!("ff: open https status={st} clen={size}"));
                            (Src::Curl(cs), size)
                        }
                        Err(e) => {
                            // A status we could not stream is worth publishing; a transport failure has
                            // no status, and 0 is how the panel already spells "never answered".
                            if let crate::curlio::OpenErr::Status(st) = e {
                                SHARED.dg_http_status.store(st, Ordering::Relaxed);
                            }
                            crate::player::log(&format!("ff: https open FAILED: {e:?}"));
                            SHARED.demux_failed.store(true, Ordering::Release);
                            break;
                        }
                    }
                } else {
                    crate::stream::http_close(hs_p);
                    if crate::stream::http_open(
                        hs_p,
                        host_c.as_ptr(),
                        port,
                        path_c.as_ptr(),
                        std::ptr::null(),
                        "GET",
                    ) != 0
                    {
                        let st = crate::stream::hs_status(hs_p);
                        SHARED.dg_http_status.store(st, Ordering::Relaxed);
                        crate::player::log(&format!("ff: http_open FAILED status={st}"));
                        SHARED.demux_failed.store(true, Ordering::Release);
                        break;
                    }
                    let size = crate::stream::hs_content_length(hs_p);
                    SHARED.file_size.store(size, Ordering::Release);
                    let st = crate::stream::hs_status(hs_p);
                    SHARED.dg_http_status.store(st, Ordering::Relaxed);
                    crate::player::log(&format!("ff: open status={st} clen={size}"));
                    (
                        Src::Socket {
                            hs: hs_p,
                            host: host_c.clone(),
                            port,
                            path: path_c.clone(),
                        },
                        size,
                    )
                };

                let mut state = Box::new(AvioState {
                    src,
                    aq: aq_p,
                    off: 0,
                    size,
                    io_failed: false,
                    body_active_us: 0,
                    body_bytes: 0,
                    first_byte_at: None,
                    reserve_deadline: ReserveDeadlineState::new(None, false),
                    transport_watchdog: None,
                    // A progressive part is not on a ladder, so there is no cheaper rung to run to
                    // and the abort rule's escape does not exist. R12's terminal case, structurally.
                    stall: None,
                    stall_aborted: false,
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
                let r = avformat_open_input(
                    &mut fmt,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
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
                // The container's own track names, published before anything else can fail: the track
                // menu is the only reader and it wants them whether or not this part turns out to have
                // a video stream. See `stream_name` for why they are read at all — for an MP4 they are
                // the ONLY place a track's identity exists, PMS having dropped it.
                {
                    let (audio, subs) = track_names(fmt);
                    let named = audio
                        .iter()
                        .chain(subs.iter())
                        .filter(|n| !n.is_empty())
                        .count();
                    if named > 0 {
                        crate::player::log(&format!(
                            "ff: container names {named}/{} tracks (a={} s={})",
                            audio.len() + subs.len(),
                            audio.len(),
                            subs.len()
                        ));
                    }
                    *SHARED.track_names.lock().unwrap() = crate::player::TrackNames { audio, subs };
                }
                let vi =
                    av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, std::ptr::null_mut(), 0);
                // native audio-track selection: feed the chosen Nth audio stream (SHARED.desired_
                // audio_idx, set by the track menu), else the pipeline's best/default audio.
                let want_aidx = SHARED.desired_audio_idx.load(Ordering::Relaxed);
                let ai = if want_aidx >= 0 {
                    nth_audio_stream(fmt, want_aidx).unwrap_or_else(|| {
                        av_find_best_stream(
                            fmt,
                            AVMEDIA_TYPE_AUDIO,
                            -1,
                            -1,
                            std::ptr::null_mut(),
                            0,
                        )
                    })
                } else {
                    // default: feed the track matching the Load payload's codec (Media[0].audioCodec),
                    // NOT av_find_best_stream — a codec mismatch stalls the audio ES and wedges video.
                    audio_stream_matching(fmt, &acodec).unwrap_or_else(|| {
                        av_find_best_stream(
                            fmt,
                            AVMEDIA_TYPE_AUDIO,
                            -1,
                            -1,
                            std::ptr::null_mut(),
                            0,
                        )
                    })
                };
                if vi < 0 {
                    // "No video stream" alone reads as a demuxer fault. When audio IS present the
                    // demuxer worked fine — the stream itself arrived without video, which on a
                    // transcode URL means the server dropped the track (no usable video target —
                    // issue #22: an HEVC-only target on a server without Plex Pass). Record which
                    // shape this is so the error the user sees can say so; the main thread words it.
                    if ai >= 0 {
                        SHARED.demux_no_video.store(true, Ordering::Release);
                        crate::player::log("ff: no video stream — the stream carries audio only");
                    } else {
                        crate::player::log("ff: no video stream");
                    }
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
                    crate::player::log(&format!(
                        "ff: AAC → ADTS reframing on (freq_idx={fi} ch={ch})"
                    ));
                }
                let dur = fmt_duration(fmt);
                if dur > 0 {
                    SHARED
                        .duration_ns
                        .store(dur.saturating_mul(1000), Ordering::Relaxed);
                }
                // The NAME as well as the id, and the name is what anything downstream should grade.
                // A raw AV_CODEC_ID is an FFmpeg enum, and that enum RENUMBERS between majors — H264
                // is 28 on the n3.3 the televisions ship, 27 on FFmpeg 6, and HEVC is 174 / 173 / 172
                // across n3.3 / 6 / 9. The on-device suite asserted the bare number, so bundling our
                // own FFmpeg failed all 21 cases at once on a codec the app had identified perfectly
                // well. `avcodec_get_name` is stable, means what the assertion meant, and is free.
                // Publish the coded size: the webOS 5+ exported window needs the frame it is being
                // fed, and this is the only place that knows it for certain — a transcode's declared
                // dimensions and its actual output need not agree.
                SHARED.publish_video_raster((*vcp).width, (*vcp).height);
                let cname =
                    std::ffi::CStr::from_ptr(avcodec_get_name((*vcp).codec_id)).to_string_lossy();
                // WHAT THE STREAM ACTUALLY IS, not just what it is called. `codec=hevc` is the same
                // four letters for an SDR file, an HDR10 file, a Dolby Vision Profile 8.1 file whose
                // base layer is that same HDR10, and a Profile 5 file that will display in visibly
                // wrong colours — `avcodec_get_name` cannot tell them apart and neither could this log
                // line or any assertion built on it. These fields can. `trc`/`pri`/`spc` are the raw
                // AVCOL_* enum values, logged as NUMBERS because naming them would mean binding three
                // more FFmpeg symbols for a diagnostic: trc 16 = smpte2084 (PQ/HDR10), 18 = arib-std-b67
                // (HLG), spc 9 = bt2020nc, pri 9 = bt2020, and **2 = UNSPECIFIED on all three** (every
                // one of those six numbers checked against the vendored `libavutil/pixfmt.h`, not
                // recalled). A Profile 5 file is expected to read 2/2/2, since IPT-PQ signals no
                // ordinary transfer at all — inferred rather than seen from here, but from the same
                // probe: PMS reports NO `colorTrc`, `colorSpace` or `colorPrimaries` at all on the dev
                // server's P5 item while sending all three on every P8 (swept 2026-08-21). This line
                // is where a television settles it.
                // All three were declared in the offsets table above and read NOWHERE until now.
                let dv = match dovi_conf(vcp) {
                    // `bl_compat` is the field that decides whether the base layer means anything on
                    // its own, so it is logged beside the profile rather than left to be inferred
                    // from it — 0 is Profile 5's "none", 1 the HDR10 of a Profile 8.1.
                    Some(d) => format!(
                        " dovi=P{} level={} bl_compat={} rpu={} el={} bl={}",
                        d.dv_profile,
                        d.dv_level,
                        d.dv_bl_signal_compatibility_id,
                        d.rpu_present_flag,
                        d.el_present_flag,
                        d.bl_present_flag
                    ),
                    None => String::new(),
                };
                crate::player::log(&format!(
                "ff: v=#{vi} codec={cname} codec_id={} {}x{} trc={} pri={} spc={}{dv} a=#{ai} dur_ns={}",
                (*vcp).codec_id,
                (*vcp).width,
                (*vcp).height,
                (*vcp).color_trc,
                (*vcp).color_primaries,
                (*vcp).color_space,
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
                        let dec = if k == SubKind::Bitmap {
                            open_sub_decoder(cp)
                        } else {
                            std::ptr::null_mut()
                        };
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
                let (param_blob, nal_len_size) = parse_extradata(
                    (*vcp).extradata,
                    (*vcp).extradata_size.max(0) as usize,
                    is_hevc,
                );
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
                        // These are content from the old seek epoch until the first new packets land.
                        // Reset both the buffer facts and the transfer-window hysteresis together so
                        // a seek cannot splice old reserve into a new low-rate observation.
                        SHARED.hls_video_tail_ns.store(-1, Ordering::Release);
                        SHARED.hls_audio_tail_ns.store(-1, Ordering::Release);
                        let ticket_refreshed = original_context.as_mut().is_some_and(
                            crate::route::AutoOriginalWatch::refresh_ticket_after_seek,
                        );
                        if ticket_refreshed {
                            let watch = original_watch
                                .as_mut()
                                .expect("an Auto Original context has its controller");
                            // A PARTIAL reset, and the split is the point: the link did not change
                            // because the viewer jumped, so the delivery estimate survives — while the
                            // buffer, the deficit history and the byte counters all describe a position
                            // that no longer exists.
                            watch.on_seek(state.body_bytes, state.body_active_us);
                            SHARED.dg_abr_net_kbps.store(-1, Ordering::Relaxed);
                            SHARED.dg_abr_buffer_ms.store(-1, Ordering::Relaxed);
                            SHARED.dg_abr_unsafe_deficit_ms.store(0, Ordering::Relaxed);
                            original_since = std::time::Instant::now();
                        } else {
                            // The route/engine moved, not merely the playhead. This worker cannot
                            // adopt that ownership; teardown will replace it.
                            original_watch = None;
                        }
                        let ts = av_rescale_q(seek_ns, NS_TB, stream_time_base(vst));
                        let sr = av_seek_frame(fmt, vi, ts, AVSEEK_FLAG_BACKWARD);
                        crate::player::log(&format!(
                            "ff: seek {}s rv={sr}",
                            seek_ns / 1_000_000_000
                        ));
                    }
                    // Pending callback errors belong only to this enclosing operation. A successful
                    // packet (possibly after an internal seek/retry) clears them; a failed operation
                    // publishes a real transport truncation to the main thread.
                    state.io_failed = false;
                    let r = av_read_frame(fmt, pkt);
                    if frame_read_failed(&mut state, r) {
                        // Genuine end of stream, teardown, or an unrecovered I/O error published by
                        // `frame_read_failed`. NOT an app seek — that is serviced at the top of this
                        // loop and never surfaces as a read error.
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
                            let head: String = aubuf[..n]
                                .iter()
                                .map(|b| format!("{:02x}", b))
                                .collect::<Vec<_>>()
                                .join(" ");
                            crate::player::log(&format!(
                                "ff: AU#0 size={} key={} head=[{}]",
                                aubuf.len(),
                                is_key,
                                head
                            ));
                        }
                        let pushed = crate::aq::aq_push(
                            aq_p,
                            aubuf.as_ptr(),
                            aubuf.len() as c_int,
                            pts,
                            if is_key { 1 } else { 0 },
                            1,
                        );
                        av_packet_unref(pkt);
                        if pushed != 0 {
                            break;
                        }
                        PUSHED_ANY.store(true, Ordering::Relaxed);
                        SHARED.hls_video_tail_ns.store(pts, Ordering::Release);
                    } else if si == ai && FEED_AUDIO.load(Ordering::Relaxed) {
                        let ast = *streams.add(ai as usize);
                        let pts = pts_ns(pkt, ast);
                        let pushed = if let Some((freq_idx, chan_cfg)) = aac_adts {
                            // prepend a 7-byte ADTS header so LG's decoder can frame the raw AAC
                            let plen = (*pkt).size as usize;
                            let mut framed = Vec::with_capacity(7 + plen);
                            framed.extend_from_slice(&adts_header(freq_idx, chan_cfg, plen));
                            framed.extend_from_slice(std::slice::from_raw_parts((*pkt).data, plen));
                            crate::aq::aq_push(
                                aqa_p,
                                framed.as_ptr(),
                                framed.len() as c_int,
                                pts,
                                1,
                                2,
                            )
                        } else {
                            crate::aq::aq_push(aqa_p, (*pkt).data, (*pkt).size, pts, 1, 2)
                            // AUDIO lane
                        };
                        av_packet_unref(pkt);
                        if pushed != 0 {
                            break;
                        }
                        SHARED.hls_audio_tail_ns.store(pts, Ordering::Release);
                    } else if let Some(sub_pos) =
                        sub_streams.iter().position(|(sidx, _, _)| *sidx == si)
                    {
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
                                    decode_bitmap_cue(
                                        dec,
                                        pkt,
                                        sub_pos as c_int,
                                        *streams.add(si as usize),
                                    );
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
                    if !SHARED.seeking.load(Ordering::Relaxed) {
                        if let Some(watch) = original_watch.as_mut() {
                            let pause_state = crate::player::TX.pause_state_sample();
                            if let Some(elapsed) =
                                original_pause_cursor.consume_completed(pause_state)
                            {
                                watch.on_resume(
                                    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                                );
                            }
                            if !pause_state.paused {
                                let audio_expected = ai >= 0 && FEED_AUDIO.load(Ordering::Relaxed);
                                if let Some(observation) = watch.observe(
                                    state.body_bytes,
                                    state.body_active_us,
                                    progressive_buffered_ms(audio_expected),
                                    remaining_playback_ms(),
                                    original_since.elapsed().as_millis() as u64,
                                ) {
                                    SHARED.dg_abr_net_kbps.store(
                                        i64::from(observation.measured_kbps),
                                        Ordering::Relaxed,
                                    );
                                    SHARED
                                        .dg_abr_buffer_ms
                                        .store(observation.buffered_ms, Ordering::Relaxed);
                                    SHARED
                                        .dg_abr_unsafe_deficit_ms
                                        .store(observation.unsafe_deficit_ms, Ordering::Relaxed);
                                    // The rest of the model, on ONE line each — `shared.rs`'s writer guard
                                    // greps `dg_<field>.store(` and a rustfmt-split call reads to it as a
                                    // field nothing writes.
                                    //
                                    // Without these, Original mode published a buffer LEVEL and left the
                                    // slope and the horizon at their reset values, so the panel drew
                                    // `+0.0 s/s · no deficit` — two sentinels, rendered as measurements,
                                    // beside a level that was real. Device-observed 2026-08-26.
                                    let rel = Ordering::Relaxed;
                                    let horizon =
                                        observation.horizon_secs.map(i64::from).unwrap_or(-1);
                                    SHARED
                                        .dg_abr_slope_ms_per_s
                                        .store(observation.slope_ms_per_s, rel);
                                    SHARED.dg_abr_starve_secs.store(horizon, rel);
                                    SHARED
                                        .dg_abr_safe_kbps
                                        .store(i64::from(observation.conservative_kbps), rel);
                                    if let Some(reason) = observation.fallback {
                                        // The whole basis of a VISIBLE switch, in one line: the rate, the
                                        // requirement it was measured against, the reserve, its direction,
                                        // how many seconds that reserve survives, and which rule fired.
                                        crate::player::log(&format!(
                                    // **`held=` and NOT `windows=`.** The value is wall
                                    // milliseconds now (N13); reusing the old label would make an
                                    // old log's `windows=2` and a new log's `windows=2` two
                                    // different quantities under one name — the exact shape the
                                    // heartbeat's `FPS=`/`loop=` rename exists to prevent.
                                    "auto: Original -> HLS {reason:?} measured={}kbps safe={}kbps need={}kbps buf={}ms slope={}ms/s starve={} held={}ms target={}kbps",
                                    observation.measured_kbps,
                                    observation.conservative_kbps,
                                    observation.requirement_kbps,
                                    observation.buffered_ms,
                                    observation.slope_ms_per_s,
                                    observation
                                        .horizon_secs
                                        .map(|s| s.to_string())
                                        .unwrap_or_else(|| "none".to_string()),
                                    observation.unsafe_deficit_ms,
                                    observation.target.map(|r| r.kbps()).unwrap_or(0),
                                ));
                                        // Hand over the CONSERVATIVE estimate, not the last window's raw
                                        // rate. The synchronized ticket also binds it to this exact engine,
                                        // route, media epoch and user contract; stale evidence is discarded
                                        // without stopping the stream or manufacturing a playback error.
                                        let publication =
                                            original_context.as_ref().map(|context| {
                                                context.request_hls_fallback(
                                                    observation.conservative_kbps.max(1),
                                                    SHARED
                                                        .playpos_ns
                                                        .load(Ordering::Relaxed)
                                                        .max(0),
                                                )
                                            });
                                        match publication {
                                            Some(crate::route::AutomaticIntentResult::Accepted) => {
                                                break
                                            }
                                            Some(crate::route::AutomaticIntentResult::Busy) => {
                                                crate::player::log(
                                            "auto: Original fallback retained behind another route transition",
                                );
                                            }
                                            Some(crate::route::AutomaticIntentResult::Stale)
                                            | None => {
                                                crate::player::log(
                                            "auto: discarded Original fallback from a superseded worker intent",
                                        );
                                                original_watch = None;
                                            }
                                        }
                                    }
                                }
                            }
                        }
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
    // A demux that ends having produced NOTHING is a failure, whatever route it took out.
    // Only two exits used to say so — the http_open failure and the panic handler — while every
    // other early `break` in the open sequence (the ABI refusal, av_malloc, avio_alloc_context,
    // avformat_alloc_context, open_input, find_stream_info, no video stream) simply set EOF and
    // returned. EOF with zero AUs is indistinguishable from a zero-length file: the pump keeps
    // waiting, `frames` stays 0, and the HUD says "Buffering…" forever with nothing in the log to
    // say why. That is precisely the report that came back from webOS 6 and 10, and it is why the
    // report could not name a cause. Failing loudly here does not fix any of those bugs; it makes
    // the next one diagnosable.
    if !PUSHED_ANY.load(Ordering::Relaxed) {
        crate::player::log("ff: demux produced no access units — treating as a failure");
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

    #[test]
    fn an_expired_candidate_is_a_common_censored_budget_not_an_ordinal_rung_failure() {
        assert_eq!(
            abr_reject_for_hls_exit(HlsExit::PrimeExpired),
            crate::abr::RejectCause::Censored,
        );
        assert_eq!(
            abr_reject_for_hls_exit(HlsExit::NotReady),
            crate::abr::RejectCause::Circumstance,
            "an ordinary PMS not-ready reply has no exhausted deadline and says nothing about a rung",
        );
    }

    #[test]
    fn a_completed_underfilled_response_is_common_evidence_not_a_structural_rung_failure() {
        assert_eq!(
            abr_reject_for_candidate_result(true, false, crate::abr::CandidateVerdict::Ready,),
            crate::abr::RejectCause::ResponseUnchanged,
            "PMS response geometry, not the different requested actuator, determines the evidence",
        );
    }

    #[test]
    fn an_incomplete_prefix_is_unknown_on_the_unqualified_debug_panel() {
        let sample = crate::abr::SegmentSample::new(
            212_992,
            258_000,
            300_000,
            2_000,
            crate::abr::BufferSnapshot {
                playback: crate::abr::MediaTimeMs(10_000),
                video_tail: crate::abr::MediaTimeMs(16_000),
                audio_tail: Some(crate::abr::MediaTimeMs(16_000)),
                audio_expected: true,
            },
        )
        .expect("valid transfer")
        .abandoned();
        assert_eq!(
            hls_abr_rate_readout(sample),
            (-1, -1, -1),
            "without a complete= column, a right-censored prefix must not look measured",
        );
    }

    #[test]
    fn an_abr_candidate_must_decode_within_the_proposed_raster() {
        assert!(hls_raster_within(1_280, 536, crate::abr::Rung::P720));
        assert!(hls_raster_within(1_280, 720, crate::abr::Rung::P720));
        assert!(!hls_raster_within(1_920, 804, crate::abr::Rung::P720));
        assert!(!hls_raster_within(0, 720, crate::abr::Rung::P720));
    }

    #[test]
    fn an_upshift_commits_only_an_observed_quality_improvement() {
        assert!(hls_upshift_strictly_improves(
            720, 404, 896_000, 720, 404, 992_000
        ));
        assert!(hls_upshift_strictly_improves(
            720, 404, 896_000, 1_920, 1_080, 16_150_000
        ));
        assert!(!hls_upshift_strictly_improves(
            720, 404, 896_000, 720, 404, 896_000
        ));
        assert!(
            !hls_upshift_strictly_improves(720, 404, 992_000, 720, 404, 923_000),
            "the live trace's 12→16 Mbps request returned the same raster at a lower declaration",
        );
        assert!(
            !hls_upshift_strictly_improves(720, 404, 992_000, 3_840, 2_160, 896_000),
            "pixels traded for fewer declared bits are not objectively ordered without a heuristic",
        );
        assert!(!hls_upshift_strictly_improves(
            0, 0, 0, 3_840, 2_160, 20_895_000
        ));
    }

    #[test]
    fn a_short_http_body_is_not_a_completed_hls_segment() {
        assert!(!hls_body_complete(400_000, 212_992));
        assert!(hls_body_complete(400_000, 400_000));
        assert!(
            hls_body_complete(-1, 212_992),
            "a close-delimited response has no byte total to compare; transport errors are separate",
        );
    }

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

    // -- parse_dovi_conf: the Dolby Vision configuration record ---------------------------
    //
    // The record is nine plain bytes, so these fixtures ARE the wire format — no builder, no
    // FFmpeg. That is the point: `dovi_conf`'s pointer walk cannot be host-tested (dlopen returns
    // None on Darwin, so a test naming it would pass without executing one line of it), but the
    // parse is where a field could be transposed, and the parse is pure.

    /// The nine bytes as the header lays them out, so a fixture reads like the spec table.
    fn dovi_bytes(profile: u8, level: u8, rpu: u8, el: u8, bl: u8, compat: u8) -> Vec<u8> {
        vec![1, 0, profile, level, rpu, el, bl, compat, 0]
    }

    /// **Profile 5** — single-layer IPT-PQ. `bl_compat = 0` ("none") is the field that says the
    /// base layer is not displayable by a decoder that ignores the RPU, and it is the whole
    /// reason this record is read at all.
    #[test]
    fn a_profile_5_record_parses_with_no_base_layer_compatibility() {
        let d = parse_dovi_conf(&dovi_bytes(5, 6, 1, 0, 1, 0)).expect("nine bytes is a record");
        assert_eq!(d.dv_profile, 5);
        assert_eq!(d.dv_bl_signal_compatibility_id, 0);
        assert_eq!(d.el_present_flag, 0);
        assert_eq!(d.rpu_present_flag, 1);
        assert_eq!(d.bl_present_flag, 1);
        assert_eq!(d.dv_level, 6);
        assert_eq!(d.dv_version_major, 1);
    }

    /// **Profile 7** — dual layer. The enhancement-layer flag is what identifies it; note the
    /// compatibility id is 6 here (the value the dev server reports for its P7 item), so a reader
    /// that only looked at `bl_compat == 0` would call this file fine.
    #[test]
    fn a_profile_7_record_parses_with_an_enhancement_layer() {
        let d = parse_dovi_conf(&dovi_bytes(7, 6, 1, 1, 1, 6)).expect("nine bytes is a record");
        assert_eq!(d.dv_profile, 7);
        assert_eq!(d.el_present_flag, 1);
        assert_eq!(
            d.dv_bl_signal_compatibility_id, 6,
            "NOT 0 — the trap this test exists to hold"
        );
    }

    /// **Profile 8.1** — the base layer IS HDR10, which `bl_compat = 1` is exactly the statement
    /// of. Nothing about this file should change behaviour anywhere.
    #[test]
    fn a_profile_8_1_record_parses_as_hdr10_compatible() {
        let d = parse_dovi_conf(&dovi_bytes(8, 6, 1, 0, 1, 1)).expect("nine bytes is a record");
        assert_eq!(d.dv_profile, 8);
        assert_eq!(d.dv_bl_signal_compatibility_id, 1);
        assert_eq!(d.el_present_flag, 0);
    }

    /// The ABSENT case, and the short one. A file with no Dolby Vision has no side-data entry at
    /// all, which `dovi_conf` reports as `None` without ever reaching here; what this pins is the
    /// other way in — a TRUNCATED payload must not be read as a partial record, because nine
    /// bytes taken out of a seven-byte allocation is a heap overread that returns a plausible
    /// profile number rather than crashing.
    #[test]
    fn a_short_record_is_not_a_partial_record() {
        assert_eq!(parse_dovi_conf(&[]), None);
        assert_eq!(
            parse_dovi_conf(&[1, 0, 5, 6, 1, 0, 1, 0]),
            None,
            "eight bytes is not nine"
        );
        // exactly nine is the boundary, and it is inclusive
        assert!(parse_dovi_conf(&dovi_bytes(5, 6, 1, 0, 1, 0)).is_some());
        // a LONGER payload is fine and expected — a future FFmpeg may append fields, and the
        // nine we read keep their meaning
        let mut long = dovi_bytes(8, 6, 1, 0, 1, 1);
        long.extend_from_slice(&[0xAA; 7]);
        assert_eq!(parse_dovi_conf(&long).map(|d| d.dv_profile), Some(8));
    }

    /// Every field at its own offset: nine distinct byte values in, nine distinct values out.
    /// A transposition of any adjacent pair — the one mistake a hand-written record parse is
    /// actually prone to — fails here and nowhere else.
    #[test]
    fn every_field_reads_from_its_own_byte() {
        let d = parse_dovi_conf(&[10, 11, 12, 13, 14, 15, 16, 17, 18]).unwrap();
        assert_eq!(d.dv_version_major, 10);
        assert_eq!(d.dv_version_minor, 11);
        assert_eq!(d.dv_profile, 12);
        assert_eq!(d.dv_level, 13);
        assert_eq!(d.rpu_present_flag, 14);
        assert_eq!(d.el_present_flag, 15);
        assert_eq!(d.bl_present_flag, 16);
        assert_eq!(d.dv_bl_signal_compatibility_id, 17);
        assert_eq!(d.dv_md_compression, 18);
    }

    // -- nal_end: the 32-bit bounds guard -------------------------------------------------

    #[test]
    fn nal_end_accepts_a_nal_that_fits() {
        assert_eq!(nal_end(4, 10, 64), Some(14));
        assert_eq!(
            nal_end(4, 60, 64),
            Some(64),
            "a NAL ending exactly at `size` is valid"
        );
    }

    #[test]
    fn nal_end_rejects_empty_and_overrun() {
        assert_eq!(
            nal_end(4, 0, 64),
            None,
            "a zero-length NAL terminates the walk"
        );
        assert_eq!(
            nal_end(4, 61, 64),
            None,
            "one byte past the end is rejected"
        );
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
        assert!(
            !old_guard_overruns,
            "on 32-bit the old guard computed 0 and let this through"
        );
        assert_eq!(
            nal_end(i, nl, size),
            None,
            "the width-explicit guard rejects it on every target"
        );
    }

    // -- packet_to_annexb ------------------------------------------------------------------

    #[test]
    fn h264_idr_is_a_keyframe_and_gets_the_parameter_set_prepended() {
        // nal_unit_type is the low 5 bits of byte 0; type 5 == IDR.
        let buf = avcc(&[&[0x65, 0xAA, 0xBB]]);
        let param = [0u8, 0, 0, 1, 0x67, 0x42];
        let (key, out) = to_annexb(&buf, false, &param);
        assert!(key, "0x65 & 0x1f == 5 is an IDR");
        assert!(
            out.starts_with(&param),
            "a keyframe AU must carry the SPS/PPS"
        );
        assert_eq!(&out[param.len()..], &[0, 0, 0, 1, 0x65, 0xAA, 0xBB]);
    }

    #[test]
    fn h264_non_idr_is_not_a_keyframe_and_gets_no_parameter_set() {
        let buf = avcc(&[&[0x41, 0x01]]); // type 1, non-IDR slice
        let (key, out) = to_annexb(&buf, false, &[0xDE, 0xAD]);
        assert!(!key);
        assert_eq!(
            out,
            vec![0, 0, 0, 1, 0x41, 0x01],
            "no parameter set on a non-keyframe"
        );
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
        assert_eq!(
            out,
            vec![0, 0, 0, 1, 0x41, 0x01],
            "the good NAL survives, the bad one stops the walk"
        );
    }

    #[test]
    fn a_runt_packet_is_rejected_before_any_indexing() {
        let (key, out) = to_annexb(&[0x00, 0x00], false, &[0xFF]);
        assert!(!key);
        assert!(out.is_empty(), "size < nls + 1 must bail before the walk");
    }

    // -- MPEG-TS elementary-stream framing -----------------------------------------------

    #[test]
    fn a_ts_idr_keeps_in_band_parameter_sets_without_avcc_conversion() {
        let packet = [
            0, 0, 0, 1, 0x67, 0x42, 0x00, // SPS
            0, 0, 1, 0x68, 0xce, // PPS
            0, 0, 0, 1, 0x65, 0xaa, // IDR
        ];
        let mut sets = H264ParamSets::default();
        let mut out = Vec::new();
        assert_eq!(ts_h264_access_unit(&packet, &mut sets, &mut out), Ok(true));
        assert_eq!(out, packet);
        assert!(!sets.sps.is_empty());
        assert!(!sets.pps.is_empty());
    }

    #[test]
    fn a_leading_missing_aac_timestamp_is_backfilled_from_frame_duration() {
        let mut stamps = [
            AudioStamp {
                au: 3,
                raw_ns: None,
                duration_ns: Some(21_333_333),
            },
            AudioStamp {
                au: 4,
                raw_ns: Some(900_000_000),
                duration_ns: Some(21_333_333),
            },
            AudioStamp {
                au: 5,
                raw_ns: None,
                duration_ns: Some(21_333_333),
            },
        ];
        assert_eq!(resolve_audio_stamps(&mut stamps), Ok(2));
        assert_eq!(stamps[0].raw_ns, Some(878_666_667));
        assert_eq!(stamps[2].raw_ns, Some(921_333_333));
    }

    #[test]
    fn a_timestamp_free_aac_segment_uses_only_its_frame_clock() {
        let mut stamps = [
            AudioStamp {
                au: 0,
                raw_ns: None,
                duration_ns: Some(20_000_000),
            },
            AudioStamp {
                au: 1,
                raw_ns: None,
                duration_ns: Some(20_000_000),
            },
            AudioStamp {
                au: 2,
                raw_ns: None,
                duration_ns: Some(20_000_000),
            },
        ];
        assert_eq!(resolve_audio_stamps(&mut stamps), Ok(3));
        assert_eq!(
            stamps.iter().map(|stamp| stamp.raw_ns).collect::<Vec<_>>(),
            [Some(0), Some(20_000_000), Some(40_000_000)]
        );
    }

    #[test]
    fn an_unanchored_aac_timestamp_hole_fails_closed() {
        let mut stamps = [
            AudioStamp {
                au: 0,
                raw_ns: Some(0),
                duration_ns: None,
            },
            AudioStamp {
                au: 1,
                raw_ns: None,
                duration_ns: None,
            },
        ];
        assert_eq!(
            resolve_audio_stamps(&mut stamps),
            Err("AAC timestamp hole has no duration anchor")
        );
    }

    #[test]
    fn a_later_ts_idr_recovers_cached_parameter_sets() {
        let first = [0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 3];
        let later = [0, 0, 1, 0x65, 4];
        let mut sets = H264ParamSets::default();
        let mut out = Vec::new();
        ts_h264_access_unit(&first, &mut sets, &mut out).unwrap();
        assert_eq!(ts_h264_access_unit(&later, &mut sets, &mut out), Ok(true));
        assert!(out.starts_with(&[0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2]));
        assert!(out.ends_with(&later));
    }

    #[test]
    fn a_ts_idr_without_any_parameter_sets_is_rejected() {
        let mut sets = H264ParamSets::default();
        let mut out = Vec::new();
        assert_eq!(
            ts_h264_access_unit(&[0, 0, 1, 0x65, 4], &mut sets, &mut out),
            Err("IDR has no in-band or cached SPS")
        );
    }

    #[test]
    fn adts_detection_accepts_a_real_header_but_not_arbitrary_ff_bytes() {
        let mut frame = adts_header(3, 2, 11).to_vec();
        frame.extend_from_slice(&[0; 11]);
        assert!(packet_has_adts(&frame));
        assert!(!packet_has_adts(&[0xff, 0x00, 0, 0, 0, 0, 0]));
        assert_eq!(adts_duration_ns(&frame), Some(21_333_333));
        assert_eq!(adts_duration_ns(&[0xff, 0x00, 0, 0, 0, 0, 0]), None);
    }

    // -- the AVIO callbacks under teardown -------------------------------------------------
    //
    // These drive `read_cb`/`seek_cb` directly, which works on the host precisely because neither
    // one touches FFmpeg: they are plain `extern "C"` fns over an `AvioState`, whose every field
    // (an HttpStream, an AuQueue, two CStrings, three integers) is ordinary Rust. So long as a
    // test stays off `av_*`, the callbacks link and run here exactly as they do on the TV.

    /// A loopback PMS stand-in that COUNTS both accepted connections and requests served — the
    /// observables that matter, since "did the callback go back to the server" is the whole
    /// question and no return value can answer it.
    ///
    /// **Why two counters.** `stream.rs` sends `Connection: close` and reopens per seek, so for
    /// the socket source a new request IS a new connection and the accept count says everything.
    /// libcurl instead keeps the connection in its multi handle's cache and REUSES it, which is
    /// what we want for a media stream — a seek that costs no TLS handshake — and it means a
    /// curl-backed seek that succeeded and one that was refused have the SAME accept count. So
    /// the curl-backed tests grade requests, and the accept count stays what it always was for
    /// the socket ones.
    ///
    /// Each connection gets its own handler thread and is served keep-alive; the accept count is
    /// bumped BEFORE the reply is written, so it is already final by the time any `http_open`
    /// against this listener can return — every assertion below is causally ordered behind that,
    /// and needs no sleep and no timing margin.
    fn with_counting_listener(
        body: impl FnOnce(u16, &std::sync::atomic::AtomicUsize, &std::sync::atomic::AtomicUsize),
    ) {
        use std::io::{Read, Write};
        use std::sync::atomic::AtomicUsize;
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        srv.set_nonblocking(true).expect("set_nonblocking"); // so the acceptor can be stopped
        let accepts = AtomicUsize::new(0);
        let requests = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                while !stop.load(Ordering::Acquire) {
                    match srv.accept() {
                        Ok((s, _)) => {
                            accepts.fetch_add(1, Ordering::AcqRel);
                            let (rq, st) = (&requests, &stop);
                            sc.spawn(move || {
                                // Read each request head, count it, answer 200 with an 8-byte
                                // body — small enough that it arrives inside the client's header
                                // read, so `http_read` serves it from `HttpStream`'s buffer and
                                // never needs the socket again.
                                let _ =
                                    s.set_read_timeout(Some(std::time::Duration::from_millis(100)));
                                let mut w = match s.try_clone() {
                                    Ok(c) => c,
                                    Err(_) => return,
                                };
                                let mut buf: Vec<u8> = Vec::new();
                                loop {
                                    if let Some(k) = buf.windows(4).position(|x| x == b"\r\n\r\n") {
                                        buf.drain(..k + 4);
                                        rq.fetch_add(1, Ordering::AcqRel);
                                        if w.write_all(
                                            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nABCDEFGH",
                                        )
                                        .is_err()
                                        {
                                            return;
                                        }
                                        let _ = w.flush();
                                        continue;
                                    }
                                    let mut tmp = [0u8; 1024];
                                    match (&s).read(&mut tmp) {
                                        Ok(0) => return,
                                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                                        Err(ref e)
                                            if matches!(
                                                e.kind(),
                                                std::io::ErrorKind::WouldBlock
                                                    | std::io::ErrorKind::TimedOut
                                            ) =>
                                        {
                                            if st.load(Ordering::Acquire) {
                                                return;
                                            }
                                        }
                                        Err(_) => return,
                                    }
                                }
                            });
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(1))
                        }
                        Err(_) => break,
                    }
                }
            });
            // Stop everything on the way out however we leave. A FAILING assertion in `body`
            // unwinds through here and `scope` joins before it reports, so a flag set only on the
            // success path would turn every real failure into a hang instead of a message.
            struct StopAcceptor<'a>(&'a AtomicBool);
            impl Drop for StopAcceptor<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            let _stop_on_exit = StopAcceptor(&stop);
            body(port, &accepts, &requests);
        });
    }

    /// An open stream plus the aborted video lane behind its AVIO — the state teardown leaves:
    /// `engine::teardown` aborts both lanes, `http_shutdown`s this socket, then joins the demuxer.
    fn opened_stream_with_aborted_lane(
        port: u16,
    ) -> (Box<HttpStream>, Box<AuQueue>, CString, CString) {
        let ip = CString::new("127.0.0.1").unwrap();
        let path = CString::new("/library/parts/1/file.mkv").unwrap();
        let mut hs = crate::stream::http_stream_boxed();
        let rv = crate::stream::http_open(
            &mut *hs,
            ip.as_ptr(),
            port as c_int,
            path.as_ptr(),
            std::ptr::null(),
            "GET",
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
        with_counting_listener(|port, accepts, _| {
            let (mut hs, mut aq, ip, path) = opened_stream_with_aborted_lane(port);
            assert_eq!(
                accepts.load(Ordering::Acquire),
                1,
                "fixture: exactly one connection so far"
            );
            let mut st = AvioState {
                src: Src::Socket {
                    hs: &mut *hs,
                    host: ip,
                    port: port as c_int,
                    path,
                },
                aq: &mut *aq,
                off: 0,
                size: 8,
                io_failed: false,
                body_active_us: 0,
                body_bytes: 0,
                first_byte_at: None,
                reserve_deadline: ReserveDeadlineState::new(None, false),
                transport_watchdog: None,
                stall: None,
                stall_aborted: false,
            };
            let op = &mut st as *mut AvioState as *mut c_void;

            let rv = seek_cb(op, 4, SEEK_SET);

            assert_eq!(
                accepts.load(Ordering::Acquire),
                1,
                "seek_cb opened a SECOND connection during teardown — the one the main thread's \
                 join is waiting on, and the one its shutdown(2) can no longer reach"
            );
            assert_eq!(
                rv, -1,
                "an aborted seek must report failure so libavformat stops healing"
            );
            assert_eq!(
                seek_cb(op, 0, AVSEEK_SIZE),
                8,
                "a size query is not I/O — the guard belongs AFTER that branch"
            );
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
        with_counting_listener(|port, accepts, _| {
            let (mut hs, mut aq, ip, path) = opened_stream_with_aborted_lane(port);
            let mut st = AvioState {
                src: Src::Socket {
                    hs: &mut *hs,
                    host: ip,
                    port: port as c_int,
                    path,
                },
                aq: &mut *aq,
                off: 0,
                size: 8,
                io_failed: false,
                body_active_us: 0,
                body_bytes: 0,
                first_byte_at: None,
                reserve_deadline: ReserveDeadlineState::new(None, false),
                transport_watchdog: None,
                stall: None,
                stall_aborted: false,
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
                accepts.load(Ordering::Acquire),
                1,
                "the read/seek recovery loop reconnected once per hop — that is the wedge, \
                 not merely a slow teardown"
            );
            assert!(
                reads.iter().all(|r| *r == AVERROR_EOF),
                "aborted reads must all report EOF: {reads:?}"
            );
            assert!(
                seeks.iter().all(|r| *r == -1),
                "every hop must refuse the seek: {seeks:?}"
            );
            crate::stream::http_close(&mut *hs);
            crate::aq::aq_destroy(&mut *aq);
        });
    }

    #[test]
    fn an_expired_candidate_deadline_stops_before_touching_its_transport() {
        let mut aq = crate::aq::aq_new(1 << 20);
        let mut st = AvioState {
            // A null stream would crash if dispatch were reached; the deadline must settle first.
            src: Src::Socket {
                hs: std::ptr::null_mut(),
                host: CString::new("unused").unwrap(),
                port: 1,
                path: CString::new("/unused").unwrap(),
            },
            aq: &mut *aq,
            off: 0,
            size: -1,
            io_failed: false,
            body_active_us: 0,
            body_bytes: 0,
            first_byte_at: None,
            reserve_deadline: ReserveDeadlineState::new(Some(std::time::Instant::now()), false),
            transport_watchdog: None,
            stall: None,
            stall_aborted: false,
        };
        let mut dst = [0u8; 8];
        let result = read_cb(
            &mut st as *mut AvioState as *mut c_void,
            dst.as_mut_ptr(),
            dst.len() as c_int,
        );
        assert_eq!(result, AVERROR_EOF);
        assert!(st.reserve_deadline.expired);
        assert!(
            !st.io_failed,
            "a rejected prime is not an active-stream transport failure"
        );
        crate::aq::aq_destroy(&mut *aq);
    }

    #[test]
    fn a_stalled_candidate_body_ends_at_transport_liveness_not_recursive_reserve_retries() {
        use std::io::{Read, Write};

        let _guard = crate::testlock::serial();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stall server");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let _ = socket.set_read_timeout(Some(std::time::Duration::from_secs(1)));
            let mut request = Vec::new();
            let mut chunk = [0u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let n = socket.read(&mut chunk).expect("read request");
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&chunk[..n]);
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n")
                .expect("headers");
            socket.flush().expect("flush headers");
            std::thread::sleep(std::time::Duration::from_millis(160));
        });

        let host = CString::new("127.0.0.1").unwrap();
        let path = CString::new("/segment.ts").unwrap();
        let mut hs = crate::stream::http_stream_boxed();
        assert_eq!(
            crate::stream::http_open(
                &mut *hs,
                host.as_ptr(),
                port as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
            ),
            0,
        );
        let start_ns = 80_000_000_000;
        let old_playpos = SHARED.playpos_ns.swap(start_ns, Ordering::AcqRel);
        let old_paused = crate::player::TX.paused.swap(false, Ordering::AcqRel);
        let mut aq = crate::aq::aq_new(1 << 20);
        let mut state = AvioState {
            src: Src::Socket {
                hs: &mut *hs,
                host,
                port: port as c_int,
                path,
            },
            aq: &mut *aq,
            off: 0,
            size: 8,
            io_failed: false,
            body_active_us: 0,
            body_bytes: 0,
            first_byte_at: None,
            reserve_deadline: ReserveDeadlineState::from_playhead_at(
                start_ns,
                std::time::Duration::from_millis(2),
                false,
            ),
            transport_watchdog: Some(TransportWatchdog::with_inactivity(
                std::time::Duration::from_millis(35),
            )),
            stall: None,
            stall_aborted: false,
        };
        let started = std::time::Instant::now();
        let mut dst = [0u8; 8];
        let result = read_cb(
            &mut state as *mut AvioState as *mut c_void,
            dst.as_mut_ptr(),
            dst.len() as c_int,
        );
        let elapsed = started.elapsed();

        assert_eq!(result, AVERROR_IO);
        assert!(state.io_failed);
        assert!(!state.reserve_deadline.expired);
        assert!(
            elapsed >= std::time::Duration::from_millis(20)
                && elapsed < std::time::Duration::from_millis(140),
            "one no-progress epoch should bound every stale reserve wake: {elapsed:?}",
        );

        SHARED.playpos_ns.store(old_playpos, Ordering::Release);
        crate::player::TX
            .paused
            .store(old_paused, Ordering::Release);
        crate::stream::http_close(&mut *hs);
        crate::aq::aq_destroy(&mut *aq);
        server.join().expect("stall server");
    }

    #[test]
    fn abort_while_a_playlist_body_is_blocked_wins_over_the_transport_result() {
        use std::io::{Read, Write};

        let _guard = crate::testlock::serial();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stall server");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            let mut chunk = [0u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let n = socket.read(&mut chunk).expect("read request");
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&chunk[..n]);
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n")
                .expect("headers");
            socket.flush().expect("flush headers");
            std::thread::sleep(std::time::Duration::from_millis(160));
        });

        let host = CString::new("127.0.0.1").unwrap();
        let path = CString::new("/playlist.m3u8").unwrap();
        let mut hs = crate::stream::http_stream_boxed();
        assert_eq!(
            crate::stream::http_open(
                &mut *hs,
                host.as_ptr(),
                port as c_int,
                path.as_ptr(),
                std::ptr::null(),
                "GET",
            ),
            0,
        );
        let mut aq = crate::aq::aq_new(1 << 20);
        let aq_addr = (&mut *aq) as *mut AuQueue as usize;
        let hs_addr = (&mut *hs) as *mut HttpStream as usize;
        let mut src = Src::Socket {
            hs: &mut *hs,
            host,
            port: port as c_int,
            path,
        };
        let mut dst = [0u8; 8];
        let outcome = std::thread::scope(|scope| {
            scope.spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(30));
                crate::aq::aq_abort(aq_addr as *mut AuQueue);
                crate::stream::http_shutdown(hs_addr as *mut HttpStream);
            });
            hls_source_read(
                &mut src,
                &mut *aq,
                &mut dst,
                Some(std::time::Instant::now() + std::time::Duration::from_secs(1)),
            )
        });

        assert!(matches!(outcome, Err(HlsExit::Aborted)), "got {outcome:?}");
        crate::stream::http_close(&mut *hs);
        crate::aq::aq_destroy(&mut *aq);
        server.join().expect("stall server");
    }

    // -- the same two invariants, with libcurl under the AVIO instead of a socket ------------
    //
    // The guards above live in `read_cb`/`seek_cb`, ABOVE the dispatch, so they are transport
    // independent by construction — which is exactly the kind of claim that stops being true the
    // first time somebody moves a check into a branch. These pin it. The listener speaks plain
    // HTTP and curl speaks plain HTTP, so no TLS is needed to grade the abort path; what a host
    // cannot reach is a real handshake, which is why the PR carries a device recipe for aborting
    // during DNS and during TLS instead of pretending these cover it.

    /// libcurl bound and both tables live, with the crate-wide lock HELD for the caller's whole
    /// test — `curlio`'s one-source registry is a process-global these two contend on with
    /// `curlio`'s own suite, in another module, which is exactly what `testlock` is for. `None`
    /// on a host with no libcurl at all, where these two would be grading nothing.
    fn curl_gate() -> Option<std::sync::MutexGuard<'static, ()>> {
        let g = crate::testlock::serial();
        if crate::net::global_init() && crate::curlio::available() {
            Some(g)
        } else {
            None
        }
    }

    /// The curl twin of `opened_stream_with_aborted_lane`: a live https-capable source over the
    /// counting listener, plus the aborted video lane teardown leaves behind.
    fn opened_curl_with_aborted_lane(port: u16) -> (Box<crate::curlio::CurlSource>, Box<AuQueue>) {
        let cs = crate::curlio::CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0)
            .expect("fixture: the first open must succeed");
        let mut aq = crate::aq::aq_new(1 << 20);
        crate::aq::aq_abort(&mut *aq);
        (cs, aq)
    }

    #[test]
    fn a_curl_seek_after_teardown_fails_instead_of_opening_a_second_connection() {
        let Some(_gate) = curl_gate() else { return };
        with_counting_listener(|port, accepts, requests| {
            let (cs, mut aq) = opened_curl_with_aborted_lane(port);
            assert_eq!(
                accepts.load(Ordering::Acquire),
                1,
                "fixture: exactly one connection so far"
            );
            assert_eq!(
                requests.load(Ordering::Acquire),
                1,
                "fixture: and exactly one request"
            );
            let mut st = AvioState {
                src: Src::Curl(cs),
                aq: &mut *aq,
                off: 0,
                size: 8,
                io_failed: false,
                body_active_us: 0,
                body_bytes: 0,
                first_byte_at: None,
                reserve_deadline: ReserveDeadlineState::new(None, false),
                transport_watchdog: None,
                stall: None,
                stall_aborted: false,
            };
            let op = &mut st as *mut AvioState as *mut c_void;

            let rv = seek_cb(op, 4, SEEK_SET);

            assert_eq!(
                requests.load(Ordering::Acquire),
                1,
                "seek_cb went back to the server through curlio during teardown — and it would do \
                 so on the CACHED connection, which is why this grades requests and not accepts"
            );
            assert_eq!(
                accepts.load(Ordering::Acquire),
                1,
                "and opened no new connection either"
            );
            assert_eq!(
                rv, -1,
                "an aborted seek must report failure so libavformat stops healing"
            );
            assert_eq!(
                seek_cb(op, 0, AVSEEK_SIZE),
                8,
                "a size query is not I/O — the guard belongs AFTER that branch, on both transports"
            );
            crate::aq::aq_destroy(&mut *aq);
        });
    }

    #[test]
    fn an_aborted_curl_read_and_seek_cannot_ping_pong_into_new_connections() {
        let Some(_gate) = curl_gate() else { return };
        with_counting_listener(|port, accepts, requests| {
            let (cs, mut aq) = opened_curl_with_aborted_lane(port);
            let mut st = AvioState {
                src: Src::Curl(cs),
                aq: &mut *aq,
                off: 0,
                size: 8,
                io_failed: false,
                body_active_us: 0,
                body_bytes: 0,
                first_byte_at: None,
                reserve_deadline: ReserveDeadlineState::new(None, false),
                transport_watchdog: None,
                stall: None,
                stall_aborted: false,
            };
            let op = &mut st as *mut AvioState as *mut c_void;
            let mut dst = [0u8; 8];
            let mut reads = Vec::new();
            let mut seeks = Vec::new();
            for _ in 0..8 {
                reads.push(read_cb(op, dst.as_mut_ptr(), dst.len() as c_int));
                seeks.push(seek_cb(op, 4, SEEK_SET));
            }
            assert_eq!(
                requests.load(Ordering::Acquire),
                1,
                "the read/seek recovery loop asked the server again once per hop — that is the \
                 wedge, not merely a slow teardown"
            );
            assert_eq!(
                accepts.load(Ordering::Acquire),
                1,
                "and it opened no new connection either"
            );
            assert!(
                reads.iter().all(|r| *r == AVERROR_EOF),
                "aborted reads must all report EOF: {reads:?}"
            );
            assert!(
                seeks.iter().all(|r| *r == -1),
                "every hop must refuse the seek: {seeks:?}"
            );
            crate::aq::aq_destroy(&mut *aq);
        });
    }

    /// A curl machinery failure is not the same event as the server finishing the file. Pin the
    /// distinction at the AVIO seam, where an earlier implementation collapsed every non-positive
    /// source result to EOF and thereby hid mid-playback truncation from both FFmpeg and the HUD.
    #[test]
    fn a_curl_transport_failure_crosses_avio_as_io_error_not_eof() {
        let Some(_gate) = curl_gate() else { return };
        with_counting_listener(|port, _, _| {
            struct ClearIoFailure;
            impl Drop for ClearIoFailure {
                fn drop(&mut self) {
                    SHARED.demux_io_failed.store(false, Ordering::Relaxed);
                }
            }
            let _clear = ClearIoFailure;
            SHARED.demux_io_failed.store(false, Ordering::Relaxed);

            let mut cs =
                crate::curlio::CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0)
                    .expect("fixture: open");
            cs.fail_multi_for_test();
            let mut aq = crate::aq::aq_new(1 << 20);
            let mut st = AvioState {
                src: Src::Curl(cs),
                aq: &mut *aq,
                off: 0,
                size: 8,
                io_failed: false,
                body_active_us: 0,
                body_bytes: 0,
                first_byte_at: None,
                reserve_deadline: ReserveDeadlineState::new(None, false),
                transport_watchdog: None,
                stall: None,
                stall_aborted: false,
            };
            let op = &mut st as *mut AvioState as *mut c_void;
            let mut dst = [0u8; 8];

            // Headers and all eight body bytes may have arrived together. Drain any buffered bytes;
            // the terminal callback result is what must retain the machinery failure.
            let mut terminal = 1;
            for _ in 0..3 {
                terminal = read_cb(op, dst.as_mut_ptr(), dst.len() as c_int);
                if terminal <= 0 {
                    break;
                }
            }

            assert_eq!(
                terminal, AVERROR_IO,
                "transport failure must not masquerade as AVERROR_EOF"
            );
            assert!(
                st.io_failed,
                "the callback leaves the error pending on its enclosing operation"
            );
            assert!(
                !SHARED.demux_io_failed.load(Ordering::Acquire),
                "a callback alone is not fatal because libavformat may still recover"
            );
            assert!(
                !frame_read_failed(&mut st, 0),
                "a recovered packet clears the pending error"
            );
            assert!(!st.io_failed);
            assert!(!SHARED.demux_io_failed.load(Ordering::Acquire));

            let terminal = read_cb(op, dst.as_mut_ptr(), dst.len() as c_int);
            assert_eq!(terminal, AVERROR_IO);
            assert!(
                frame_read_failed(&mut st, terminal),
                "an unrecovered frame read ends the demux loop"
            );
            assert!(
                SHARED.demux_io_failed.load(Ordering::Acquire),
                "the main-thread pump must see the failure even after frames were presented"
            );
            crate::aq::aq_destroy(&mut *aq);
        });
    }
}

#[cfg(test)]
mod hls_recovery_priority_tests {
    use super::{
        candidate_reserve_deadline, classify_hls_avio_facts, classify_hls_deadline,
        classify_plaintext_open_failure, exploration_snapshot, exploration_timeout_is_final,
        hls_candidate_disposition, hls_candidate_requires_rung_box, hls_duration_obligation_ms,
        hls_quality_trial_may_start, hls_wait, latched_original_recovery, observe_hls_deadline,
        HlsCandidateDisposition, HlsCandidateTransition, HlsExit, LatchedOriginalRecovery,
        ReserveDeadlineState, SegmentTransfer, TransportWatchdog, SHARED,
    };

    #[test]
    fn fractional_hls_duration_is_rounded_up_as_a_reserve_obligation() {
        assert_eq!(
            hls_duration_obligation_ms(std::time::Duration::from_micros(551_044)),
            Some(552),
        );
        assert_eq!(
            hls_duration_obligation_ms(std::time::Duration::from_millis(2_000)),
            Some(2_000),
        );
    }
    use crate::abr::Direction;

    #[test]
    fn a_terminal_floor_response_cannot_loop_only_because_pms_exceeded_its_box() {
        let floor = crate::abr::Proposal {
            rung: crate::abr::Rung::P240,
            direction: Direction::Down,
        };
        assert!(!hls_candidate_requires_rung_box(
            floor,
            crate::abr::ReservePolicy::TerminalFloor,
        ));
        assert!(hls_candidate_requires_rung_box(
            floor,
            crate::abr::ReservePolicy::Preserve,
        ));
    }

    #[test]
    fn a_quality_trial_never_extends_an_active_rebuffer_hold() {
        assert!(hls_quality_trial_may_start(
            Direction::Up,
            false,
            false,
            false
        ));
        assert!(!hls_quality_trial_may_start(
            Direction::Up,
            true,
            false,
            false
        ));
        assert!(!hls_quality_trial_may_start(
            Direction::Up,
            false,
            true,
            false
        ));
        assert!(!hls_quality_trial_may_start(
            Direction::Up,
            true,
            true,
            false
        ));
    }

    #[test]
    fn an_abort_at_the_exact_wait_boundary_wins_over_reserve_expiry() {
        let mut aq = crate::aq::aq_new(1 << 20);
        crate::aq::aq_abort(&mut *aq);
        let result = hls_wait(
            &mut *aq,
            std::time::Duration::ZERO,
            Some(std::time::Instant::now()),
        );
        assert!(matches!(result, Err(HlsExit::Aborted)));
        crate::aq::aq_destroy(&mut *aq);
    }

    #[test]
    fn a_downshift_remains_available_because_it_is_the_recovery_edge() {
        assert!(hls_quality_trial_may_start(
            Direction::Down,
            true,
            true,
            false
        ));
    }

    #[test]
    fn teardown_wins_over_an_expired_plaintext_open_snapshot() {
        assert!(matches!(
            classify_plaintext_open_failure(crate::stream::HttpOpenError::Aborted),
            HlsExit::Aborted
        ));
    }

    #[test]
    fn every_ffmpeg_phase_reduces_callback_facts_with_one_priority_table() {
        let transfer = SegmentTransfer {
            bytes: 123,
            active_us: 456,
            total_us: 789,
            audio_expected: true,
        };
        assert!(matches!(
            classify_hls_avio_facts(Some(transfer), true, true, true),
            Some(HlsExit::StallAbort(observed)) if observed == transfer
        ));
        assert!(matches!(
            classify_hls_avio_facts(None, true, true, true),
            Some(HlsExit::PrimeExpired)
        ));
        assert!(matches!(
            classify_hls_avio_facts(None, false, true, true),
            Some(HlsExit::Failed("segment body transport failed"))
        ));
        assert!(
            classify_hls_avio_facts(None, false, true, false).is_none(),
            "libavformat recovery clears a callback error instead of inventing a terminal",
        );
    }

    #[test]
    fn rejected_candidate_media_has_only_the_discard_transition() {
        assert_eq!(
            hls_candidate_disposition(true, true, crate::abr::CandidateVerdict::Unfunded,),
            HlsCandidateDisposition::Discard {
                cause: crate::abr::RejectCause::Candidate,
                outcome: "not_ready_discarded",
            },
        );
        assert_eq!(
            hls_candidate_disposition(true, true, crate::abr::CandidateVerdict::Ready),
            HlsCandidateDisposition::Commit,
        );
    }

    #[test]
    fn a_user_pause_fills_the_active_queues_without_starting_a_private_trial() {
        assert!(!hls_quality_trial_may_start(
            Direction::Up,
            false,
            false,
            true
        ));
        assert!(!hls_quality_trial_may_start(
            Direction::Down,
            false,
            false,
            true
        ));
    }

    #[test]
    fn a_latched_original_switch_preempts_an_unstarted_hls_prime() {
        let proposal = crate::abr::Proposal {
            rung: crate::abr::Rung::P1080M12,
            direction: Direction::Up,
        };
        assert_eq!(
            latched_original_recovery(
                24_000,
                None,
                crate::abr::Decision::Prime(proposal),
                true,
                Some(2_000),
                2_000,
            ),
            LatchedOriginalRecovery::Switch {
                kbps: 24_000,
                cancel: Some(proposal),
            },
        );
        assert_eq!(
            latched_original_recovery(
                24_000,
                None,
                crate::abr::Decision::Prime(proposal),
                false,
                Some(2_000),
                2_000,
            ),
            LatchedOriginalRecovery::Wait,
            "a live cleanup obligation still keeps both actuator transitions quiescent",
        );
    }

    /// Device regression, 2026-09-01: the raw-Part source probe completed with `Recover`, then
    /// PMS returned 404 for every later segment under the shared Streaming Resource.  The old
    /// loop copied the result into `recover_kbps`, continued, and could publish it only after one
    /// more successful HLS object — an impossible transition on that server.  A fresh verdict is
    /// already on the completed media boundary and must therefore produce `Switch` immediately.
    #[test]
    fn a_fresh_original_probe_switches_on_its_own_media_boundary() {
        assert_eq!(
            latched_original_recovery(
                0,
                Some(49_041),
                crate::abr::Decision::Stay,
                true,
                Some(4_000),
                2_000,
            ),
            LatchedOriginalRecovery::Switch {
                kbps: 49_041,
                cancel: None,
            },
        );
    }

    #[test]
    fn a_post_feed_transition_defaults_to_fenced_until_teardown() {
        let _guard = crate::testlock::serial();
        let stable = SHARED
            .hls_candidate_generation
            .load(std::sync::atomic::Ordering::Acquire);
        assert_eq!(stable & 1, 0, "test starts outside a transition");
        struct Restore(u64);
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .hls_candidate_generation
                    .store(self.0, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore(stable.wrapping_add(2));

        drop(HlsCandidateTransition::arm_unowned_for_test().expect("single transition"));
        assert!(
            SHARED
                .hls_candidate_generation
                .load(std::sync::atomic::Ordering::Acquire)
                & 1
                != 0,
            "dropping a fatal partial transition reopened Play before queue teardown",
        );
    }

    #[test]
    fn proven_candidate_transition_settlement_reopens_generation() {
        let _guard = crate::testlock::serial();
        let stable = SHARED
            .hls_candidate_generation
            .load(std::sync::atomic::Ordering::Acquire);
        assert_eq!(stable & 1, 0, "test starts outside a transition");
        struct Restore(u64);
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .hls_candidate_generation
                    .store(self.0, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore(stable.wrapping_add(2));

        HlsCandidateTransition::arm_unowned_for_test()
            .expect("single transition")
            .settle();
        assert_eq!(
            SHARED
                .hls_candidate_generation
                .load(std::sync::atomic::Ordering::Acquire),
            stable.wrapping_add(2),
            "explicit settlement did not publish the next stable generation",
        );
    }

    #[test]
    fn unwind_after_candidate_media_publication_stays_fenced_until_teardown() {
        let _guard = crate::testlock::serial();
        let stable = SHARED
            .hls_candidate_generation
            .load(std::sync::atomic::Ordering::Acquire);
        assert_eq!(stable & 1, 0, "test starts outside a transition");
        struct Restore(u64);
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .hls_candidate_generation
                    .store(self.0, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore(stable.wrapping_add(2));

        let unwind = std::panic::catch_unwind(|| {
            let _transition =
                HlsCandidateTransition::arm_unowned_for_test().expect("single transition");
            panic!("candidate publication unwound before commit or cursor realignment");
        });
        assert!(unwind.is_err());
        assert!(
            SHARED
                .hls_candidate_generation
                .load(std::sync::atomic::Ordering::Acquire)
                & 1
                != 0,
            "unwind made a partially-published candidate generation look stable",
        );
    }

    #[test]
    fn a_floor_deadline_can_only_promote_while_the_fetch_is_in_flight() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut floor = ReserveDeadlineState::new(Some(deadline), true);
        assert_eq!(
            floor.active(false),
            Some(deadline),
            "a bounded floor rollback starts with its proved reserve deadline",
        );
        assert_eq!(
            floor.active(true),
            None,
            "once the internal clock is held, the already-spent rollback deadline disappears",
        );
        assert_eq!(
            floor.active(false),
            None,
            "a later Resume cannot resurrect reserve already released by this transaction",
        );

        let mut ordinary = ReserveDeadlineState::new(Some(deadline), false);
        assert_eq!(
            ordinary.active(true),
            Some(deadline),
            "an upshift or a non-floor downshift cannot inherit the floor exception",
        );
    }

    #[test]
    fn a_hold_racing_a_blocking_timeout_releases_instead_of_expiring_the_floor() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut floor = ReserveDeadlineState::new(Some(deadline), true);
        let attempted_deadline = floor.active(false);

        assert!(attempted_deadline.is_some());
        assert!(
            !floor.note_transport_deadline(attempted_deadline, true),
            "the timeout belongs to the superseded rollback deadline, not transport liveness",
        );
        assert!(!floor.expired);
        assert_eq!(
            floor.active(false),
            None,
            "promotion is monotone even after the main-thread hold later clears",
        );

        let expired_at = std::time::Instant::now();
        let mut ordinary = ReserveDeadlineState::new(Some(expired_at), false);
        assert!(ordinary.note_transport_deadline(Some(expired_at), true));
        assert!(
            ordinary.expired,
            "ordinary candidates retain the deadline result"
        );
    }

    #[test]
    fn a_user_pause_does_not_spend_media_reserve_but_the_playhead_does() {
        let _guard = crate::testlock::serial();
        let start_ns = 40_000_000_000;
        let old = SHARED
            .playpos_ns
            .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
        let old_paused = crate::player::TX
            .paused
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        struct Restore {
            playpos_ns: i64,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .playpos_ns
                    .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
                crate::player::TX
                    .paused
                    .store(self.paused, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore {
            playpos_ns: old,
            paused: old_paused,
        };
        let mut reserve = candidate_reserve_deadline(
            None,
            Some(std::time::Duration::from_millis(1)),
            false,
            start_ns,
            Direction::Up,
        );
        let attempted_deadline = reserve.active(false).expect("one millisecond reserve");

        crate::player::TX
            .paused
            .store(true, std::sync::atomic::Ordering::Release);
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(
            !reserve.note_transport_deadline(Some(attempted_deadline), false),
            "elapsed wall time cannot spend reserve while the presentation clock is frozen",
        );
        assert!(!reserve.expired);
        crate::player::TX
            .paused
            .store(false, std::sync::atomic::Ordering::Release);
        assert!(
            reserve.active(false).is_some(),
            "Resume restores the same unspent playhead balance",
        );

        SHARED
            .playpos_ns
            .store(start_ns + 1_000_000, std::sync::atomic::Ordering::Release);
        assert!(
            reserve.expire_if_due(false),
            "advancing the playhead by exactly B must spend exactly B",
        );
    }

    #[test]
    fn a_pause_cannot_revive_reserve_already_spent_by_the_playhead() {
        let _guard = crate::testlock::serial();
        let start_ns = 50_000_000_000;
        let old = SHARED
            .playpos_ns
            .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
        let old_paused = crate::player::TX
            .paused
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        struct Restore {
            playpos_ns: i64,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .playpos_ns
                    .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
                crate::player::TX
                    .paused
                    .store(self.paused, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore {
            playpos_ns: old,
            paused: old_paused,
        };

        let budget = std::time::Duration::from_millis(1);
        let mut polled = ReserveDeadlineState::from_playhead(budget, false);
        let mut blocked = ReserveDeadlineState::from_playhead(budget, false);
        let attempted_deadline = blocked.active(false).expect("running reserve snapshot");

        SHARED
            .playpos_ns
            .store(start_ns + 1_000_000, std::sync::atomic::Ordering::Release);
        crate::player::TX
            .paused
            .store(true, std::sync::atomic::Ordering::Release);

        assert!(
            polled.expire_if_due(false),
            "Pause freezes only a positive balance; equality is already terminal",
        );
        assert!(
            blocked.note_transport_deadline(Some(attempted_deadline), false),
            "an in-flight snapshot remains final when the playhead spent the budget before Pause",
        );
        assert!(polled.expired && blocked.expired);
    }

    #[test]
    fn wall_time_between_projection_and_classification_cannot_spend_playhead_reserve() {
        let _guard = crate::testlock::serial();
        let start_ns = 60_000_000_000;
        let old = SHARED
            .playpos_ns
            .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
        let old_paused = crate::player::TX
            .paused
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        struct Restore {
            playpos_ns: i64,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .playpos_ns
                    .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
                crate::player::TX
                    .paused
                    .store(self.paused, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore {
            playpos_ns: old,
            paused: old_paused,
        };

        let mut reserve =
            ReserveDeadlineState::from_playhead(std::time::Duration::from_nanos(1), false);
        let attempted_deadline = reserve
            .active(false)
            .expect("positive balance projects a wake");
        std::thread::yield_now();
        assert!(
            !reserve.note_transport_deadline(Some(attempted_deadline), false),
            "an expired wall projection is obsolete while Δplayhead remains strictly below E",
        );
        assert!(!reserve.expired);
    }

    #[test]
    fn stale_reserve_wakes_cannot_renew_transport_liveness() {
        let _guard = crate::testlock::serial();
        let start_ns = 65_000_000_000;
        let old = SHARED
            .playpos_ns
            .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
        let old_paused = crate::player::TX
            .paused
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        struct Restore {
            playpos_ns: i64,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .playpos_ns
                    .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
                crate::player::TX
                    .paused
                    .store(self.paused, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore {
            playpos_ns: old,
            paused: old_paused,
        };

        let mut reserve = ReserveDeadlineState::from_playhead_at(
            start_ns,
            std::time::Duration::from_millis(2),
            false,
        );
        let watchdog = TransportWatchdog::with_inactivity(std::time::Duration::from_millis(18));
        for _ in 0..2 {
            let snapshot = reserve.active(false).expect("running projection");
            let attempted = watchdog.effective(Some(snapshot));
            std::thread::sleep(std::time::Duration::from_millis(3));
            let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
            assert!(
                classify_hls_deadline(wake).is_none(),
                "a stale reserve wake is retryable before independent liveness expires",
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
        let attempted = watchdog.effective(reserve.active(false));
        let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
        let outcome = classify_hls_deadline(wake);
        assert!(matches!(outcome, Some(HlsExit::Failed(_))));
        assert!(
            !reserve.expired,
            "transport failure is not censored rung evidence"
        );
    }

    #[test]
    fn spent_playhead_reserve_wins_before_live_transport_watchdog() {
        let _guard = crate::testlock::serial();
        let start_ns = 66_000_000_000;
        let old = SHARED
            .playpos_ns
            .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
        struct Restore(i64);
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .playpos_ns
                    .store(self.0, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore(old);
        let mut reserve = ReserveDeadlineState::from_playhead_at(
            start_ns,
            std::time::Duration::from_millis(1),
            false,
        );
        let watchdog = TransportWatchdog::with_inactivity(std::time::Duration::from_secs(1));
        let attempted = watchdog.effective(reserve.active(false));
        SHARED
            .playpos_ns
            .store(start_ns + 1_000_000, std::sync::atomic::Ordering::Release);
        let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
        assert!(matches!(
            classify_hls_deadline(wake),
            Some(HlsExit::PrimeExpired)
        ));
        assert!(reserve.expired);
        assert!(!watchdog.expired());
    }

    #[test]
    fn an_earlier_liveness_boundary_stays_transport_even_if_reserve_spends_before_classification() {
        let _guard = crate::testlock::serial();
        let start_ns = 67_000_000_000;
        let old = SHARED
            .playpos_ns
            .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
        struct Restore(i64);
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .playpos_ns
                    .store(self.0, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore(old);
        let mut reserve = ReserveDeadlineState::from_playhead_at(
            start_ns,
            std::time::Duration::from_millis(20),
            false,
        );
        let watchdog = TransportWatchdog::with_inactivity(std::time::Duration::from_millis(1));
        let attempted = watchdog.effective(reserve.active(false));

        std::thread::sleep(std::time::Duration::from_millis(2));
        let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
        SHARED
            .playpos_ns
            .store(start_ns + 20_000_000, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            classify_hls_deadline(wake),
            Some(HlsExit::Failed(_))
        ));
        assert!(
            !reserve.expired,
            "a later reserve observation cannot relabel the earlier transport boundary",
        );
    }

    #[test]
    fn bounded_downshift_spends_internal_hold_but_excludes_a_hidden_user_pause_cycle() {
        let _guard = crate::testlock::serial();
        let old_paused = crate::player::TX
            .paused
            .load(std::sync::atomic::Ordering::Acquire);
        crate::player::TX.commit_paused(false);
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::player::TX.commit_paused(self.0);
            }
        }
        let _restore = Restore(old_paused);

        let mut reserve = ReserveDeadlineState::from_unpaused_elapsed(
            std::time::Duration::from_millis(80),
            false,
        );
        let attempted = reserve.active(false).expect("bounded recovery deadline");
        std::thread::sleep(std::time::Duration::from_millis(20));
        crate::player::TX.commit_paused(true);
        std::thread::sleep(std::time::Duration::from_millis(80));
        crate::player::TX.commit_paused(false);

        assert!(
            !reserve.note_transport_deadline(Some(attempted), true),
            "a complete accepted Pause->Resume inside one blocked call spends no recovery budget",
        );
        assert!(!reserve.expired);

        // `rebuffering=true` models B=0 with the native clock held. A non-floor recovery remains
        // bounded and therefore spends this involuntary stall even though Δplayhead is zero.
        std::thread::sleep(std::time::Duration::from_millis(70));
        assert!(reserve.expire_if_due(true));
        assert!(reserve.expired);
    }

    /// Device regression, 2026-09-01. A 16→18 Mbps exploration armed 3.251 s, spent 1.421 s in
    /// control, then the viewer paused during media warm-up. The old code replaced the transaction
    /// clock with the remaining wall instant and emitted `warmup_deadline` after 3.337 s while the
    /// buffer moved only 5.251→4.834 s. The SAME playhead clock must cross control and media.
    #[test]
    fn a_pause_cannot_turn_an_upshift_control_snapshot_into_a_media_deadline() {
        let _guard = crate::testlock::serial();
        let start_ns = 70_000_000_000;
        let old = SHARED
            .playpos_ns
            .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
        let old_paused = crate::player::TX
            .paused
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        struct Restore {
            playpos_ns: i64,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .playpos_ns
                    .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
                crate::player::TX
                    .paused
                    .store(self.paused, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore {
            playpos_ns: old,
            paused: old_paused,
        };

        let mut exploration = Some(ReserveDeadlineState::from_playhead(
            std::time::Duration::from_millis(1),
            false,
        ));
        let control_snapshot = exploration_snapshot(&mut exploration).expect("running clock");
        crate::player::TX
            .paused
            .store(true, std::sync::atomic::Ordering::Release);
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(
            !exploration_timeout_is_final(&mut exploration, Some(control_snapshot)),
            "the expired wall snapshot cannot censor an unspent playhead budget",
        );

        let mut media = candidate_reserve_deadline(
            exploration,
            // A fresh media-only clock would expire immediately; carrying the exploration state
            // is therefore the differential, not merely another direct test of `from_playhead`.
            Some(std::time::Duration::ZERO),
            false,
            start_ns,
            Direction::Up,
        );
        assert!(!media.expired);
        assert!(
            media.active(false).is_some(),
            "Pause retains a scheduled re-check for a Resume that races the blocked read"
        );
        crate::player::TX
            .paused
            .store(false, std::sync::atomic::Ordering::Release);
        assert!(
            media.active(false).is_some(),
            "Resume restores the original balance"
        );
        SHARED
            .playpos_ns
            .store(start_ns + 1_000_000, std::sync::atomic::Ordering::Release);
        assert!(
            media.expire_if_due(false),
            "only Δplayhead spends the exploration"
        );
    }

    #[test]
    fn resume_during_a_blocked_pause_projection_cannot_overspend_reserve() {
        let _guard = crate::testlock::serial();
        let start_ns = 71_000_000_000;
        let old_pos = SHARED
            .playpos_ns
            .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
        let old_paused = crate::player::TX
            .paused
            .swap(true, std::sync::atomic::Ordering::AcqRel);
        struct Restore {
            playpos_ns: i64,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .playpos_ns
                    .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
                crate::player::TX
                    .paused
                    .store(self.paused, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore {
            playpos_ns: old_pos,
            paused: old_paused,
        };

        let mut reserve = ReserveDeadlineState::from_playhead_at(
            start_ns,
            std::time::Duration::from_millis(100),
            false,
        );
        let watchdog = TransportWatchdog::with_inactivity(std::time::Duration::from_secs(1));
        let attempted = reserve
            .active(false)
            .expect("a paused issue still schedules the unchanged reserve boundary");
        let attempted = watchdog.effective(Some(attempted));

        crate::player::TX
            .paused
            .store(false, std::sync::atomic::Ordering::Release);
        SHARED
            .playpos_ns
            .store(start_ns + 100_000_000, std::sync::atomic::Ordering::Release);
        let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
        assert!(matches!(
            classify_hls_deadline(wake),
            Some(HlsExit::PrimeExpired)
        ));
        assert!(!watchdog.expired());
    }

    #[test]
    fn a_pause_projection_spends_neither_reserve_nor_transport_liveness() {
        let _guard = crate::testlock::serial();
        let start_ns = 72_000_000_000;
        let old_pos = SHARED
            .playpos_ns
            .swap(start_ns, std::sync::atomic::Ordering::AcqRel);
        let old_paused = crate::player::TX
            .paused
            .swap(true, std::sync::atomic::Ordering::AcqRel);
        struct Restore {
            playpos_ns: i64,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                SHARED
                    .playpos_ns
                    .store(self.playpos_ns, std::sync::atomic::Ordering::Release);
                crate::player::TX
                    .paused
                    .store(self.paused, std::sync::atomic::Ordering::Release);
            }
        }
        let _restore = Restore {
            playpos_ns: old_pos,
            paused: old_paused,
        };

        let mut reserve = ReserveDeadlineState::from_playhead_at(
            start_ns,
            std::time::Duration::from_millis(1),
            false,
        );
        let watchdog = TransportWatchdog::with_inactivity(std::time::Duration::from_millis(50));
        let liveness_deadline = watchdog.deadline;
        let attempted = reserve.active(false).expect("paused reserve projection");
        let attempted = watchdog.effective(Some(attempted));
        std::thread::sleep(std::time::Duration::from_millis(2));
        let wake = observe_hls_deadline(Some(&mut reserve), attempted, &watchdog, false);
        assert!(classify_hls_deadline(wake).is_none());
        assert!(!reserve.expired);
        assert_eq!(watchdog.deadline, liveness_deadline);
    }
}

#[cfg(test)]
mod stall_guard_tests {
    use super::{arm_active_stall_guard, StallGuard};

    /// A completed PMS HLS object is not necessarily delivered at a stationary rate.  The server
    /// reports `segmentWait` inside downloads while its JIT encoder catches up, then sends the
    /// already-sized remainder as a burst.  On the television the first 214 440-byte prefix of a
    /// roughly 6 MB 4K object took about 258 ms, while the adjacent complete objects landed in
    /// 1.2--1.4 s.  Extrapolating that prefix over the unseen remainder predicts more than the
    /// available 6.4 s reserve and aborts a stream whose complete acquisitions are sustainable.
    ///
    /// The prefix is right-censored evidence: without a model of PMS's future production it can
    /// prove only what has already been spent, not how long unseen bytes will take.  Keeping the
    /// playhead fixed makes this differential: the broken attained-rate forecast fires; the
    /// physical reserve has spent nothing and therefore cannot.
    #[test]
    fn a_server_paced_prefix_cannot_forecast_its_unseen_remainder() {
        let _serial = crate::testlock::serial();
        let pos = 5_000_000_000i64;
        crate::player::SHARED
            .playpos_ns
            .store(pos, std::sync::atomic::Ordering::Relaxed);
        let guard = StallGuard::arm(6_376).expect("a live reserve arms");
        assert!(
            !guard.should_abort(5_785_560, false),
            "a censored JIT prefix cannot prove that the complete response will miss the reserve",
        );
    }

    /// **But a reserve that was empty when the fetch STARTED never arms the guard at all**, which
    /// is a different question and is the one that matters: that is every session's first segment,
    /// and arming there aborts the fetch that would have created the picture. Device-measured
    /// without this gate: `stall abort seq=0 ... of 0ms reserve`, playback never started, the video
    /// plane never bound.
    #[test]
    fn an_empty_reserve_at_the_start_of_a_fetch_arms_nothing() {
        assert!(
            StallGuard::arm(0).is_none(),
            "so the guard must refuse to be armed"
        );
        assert!(StallGuard::arm(-1).is_none());
        assert!(
            StallGuard::arm(1).is_some(),
            "and must arm as soon as there is a picture to keep"
        );
    }

    #[test]
    fn the_floor_guard_holds_the_clock_without_abandoning_the_only_rung() {
        let hold = StallGuard::arm_clock_hold(2_000).expect("a live floor reserve arms");
        assert!(
            !hold.aborts_fetch(),
            "the floor has no cheaper object to re-fetch"
        );
        assert!(
            StallGuard::arm(2_000)
                .expect("a live non-floor reserve arms")
                .aborts_fetch(),
            "above the floor the same signal must still release the controller to downshift",
        );
    }

    #[test]
    fn an_existing_terminal_hold_arms_no_second_abort() {
        assert!(
            arm_active_stall_guard(Some(2_000), false, true).is_none(),
            "the response rebuilding an empty reserve must be allowed to complete",
        );
        assert!(
            arm_active_stall_guard(Some(2_000), false, false).is_some(),
            "a running clock still needs its terminal boundary",
        );
    }

    /// The reserve is spent by the PLAYHEAD, not by wall time. A server may wait arbitrarily while
    /// playback is paused or re-priming without consuming one millisecond of queued media.
    #[test]
    fn a_fetch_the_playhead_is_not_consuming_never_aborts() {
        let _serial = crate::testlock::serial();
        let pos = 5_000_000_000i64;
        crate::player::SHARED
            .playpos_ns
            .store(pos, std::sync::atomic::Ordering::Relaxed);
        let guard = StallGuard {
            reserve_ms_at_start: 2_000,
            playhead_at_start_ns: pos,
            action: super::StallAction::AbortFetch,
        };
        assert!(
            !guard.should_abort(400_000, false),
            "the guard abandoned a fetch while the picture was not consuming its reserve"
        );
    }

    /// The other half, so the fix cannot be "never abort": a playhead that IS advancing spends the
    /// reserve exactly as before, and a fetch that provably cannot land still aborts.
    #[test]
    fn a_fetch_the_playhead_is_consuming_still_aborts() {
        let _serial = crate::testlock::serial();
        let pos = 5_000_000_000i64;
        let guard = StallGuard {
            reserve_ms_at_start: 2_000,
            playhead_at_start_ns: pos,
            action: super::StallAction::AbortFetch,
        };
        crate::player::SHARED
            .playpos_ns
            .store(pos + 1_999_000_000, std::sync::atomic::Ordering::Relaxed);
        assert!(
            !guard.should_abort(400_000, false),
            "one millisecond of reserve remains"
        );
        // Exactly the two seconds of media present at the boundary have now been consumed.
        crate::player::SHARED
            .playpos_ns
            .store(pos + 2_000_000_000, std::sync::atomic::Ordering::Relaxed);
        assert!(
            guard.should_abort(400_000, false),
            "a genuinely exhausted reserve must still abort"
        );
    }

    /// The main thread's B=0 hold and the worker's playhead sample are the same physical boundary.
    /// The explicit signal covers millisecond quantisation and a callback that resumes just after
    /// Starfish was paused.
    #[test]
    fn a_terminal_hold_aborts_only_an_incomplete_response() {
        let _serial = crate::testlock::serial();
        crate::player::SHARED
            .playpos_ns
            .store(5_000_000_000, std::sync::atomic::Ordering::Relaxed);
        let guard = StallGuard::arm(6_000).expect("a live reserve arms");
        assert!(
            guard.should_abort(1, true),
            "B=0 with bytes remaining is terminal"
        );
        assert!(
            !guard.should_abort(0, true),
            "a complete sized response must be credited"
        );
        assert!(
            !guard.should_abort(-1, true),
            "past the declared end is complete too"
        );
    }
}
