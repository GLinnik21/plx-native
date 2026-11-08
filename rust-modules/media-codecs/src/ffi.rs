//! One FFmpeg 9.0 ABI table and runtime loader for the bundled libraries.
#![allow(dead_code)]
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

static ABI_OK: AtomicBool = AtomicBool::new(false);
static SWS_OK: AtomicBool = AtomicBool::new(false);

// ---- opaque handles (pointer-only, never dereferenced) ----
pub enum AVClass {}
pub enum AVInputFormat {}
pub enum AVIOContext {}
pub enum AVDictionary {}
pub enum AVCodec {}
pub enum AVCodecContext {} // opaque: we only pass the lib-allocated pointer to decode/free
pub enum AVBitStreamFilter {}
pub enum AVBufferRef {}
// AVStream is opaque here: read only the fields needed at the FFmpeg 9.0 offsets
// asserted against the bundled headers by ci/ffabi-assert.c.
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
pub const OFF_STREAM_INDEX: usize = 4; // NB not 0 — FFmpeg 5.0 put `const AVClass *av_class` first
#[cfg(target_pointer_width = "32")]
pub const OFF_STREAM_CODECPAR: usize = 12;
#[cfg(target_pointer_width = "32")]
pub const OFF_STREAM_TIME_BASE: usize = 20;
#[cfg(target_pointer_width = "64")]
pub const OFF_STREAM_INDEX: usize = 8;
#[cfg(target_pointer_width = "64")]
pub const OFF_STREAM_CODECPAR: usize = 16;
#[cfg(target_pointer_width = "64")]
pub const OFF_STREAM_TIME_BASE: usize = 32;
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
pub const OFF_STREAM_METADATA: usize = 72;
#[cfg(target_pointer_width = "64")]
pub const OFF_STREAM_METADATA: usize = 80;
/// `AVFormatContext.duration`, in AV_TIME_BASE units. By offset — see the struct's closing note.
#[cfg(target_pointer_width = "32")]
pub const OFF_FMT_DURATION: usize = 64;
#[cfg(target_pointer_width = "64")]
pub const OFF_FMT_DURATION: usize = 104;
/// `AVFormatContext.start_time`, in AV_TIME_BASE units: the `int64` right before `duration`.
#[cfg(target_pointer_width = "32")]
pub const OFF_FMT_START_TIME: usize = 56;
#[cfg(target_pointer_width = "64")]
pub const OFF_FMT_START_TIME: usize = 96;

/// `AVDictionaryEntry` — two `char *`. Modelled rather than opaque because the whole point of the
/// call is to read both halves; `ci/ffabi-assert.c` holds the layout.
#[repr(C)]
pub struct AVDictionaryEntry {
    pub key: *const c_char,
    pub value: *const c_char,
}

/// `AVFormatContext.duration`, one of the two fields this app reads past `AVFormatContext.streams`.
#[inline]
pub unsafe fn fmt_duration(fmt: *const AVFormatContext) -> i64 {
    *((fmt as *const u8).add(OFF_FMT_DURATION) as *const i64)
}

/// `AVFormatContext.start_time` (the other): where the earliest stream's clock starts, or
/// `AV_NOPTS_VALUE`. Valid once `avformat_find_stream_info` has returned.
#[inline]
pub unsafe fn fmt_start_time(fmt: *const AVFormatContext) -> i64 {
    *((fmt as *const u8).add(OFF_FMT_START_TIME) as *const i64)
}

// ---- bundled FFmpeg 9.0 structs; 32-bit ARM/AAPCS sizes unless noted ----
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
// identical on 3.3 and 4.x. `duration` and `start_time` — the only fields past that point this app
// reads — are reached through [`fmt_duration`] and [`fmt_start_time`] at the offsets the runtime ABI
// table gives, rather than by declaring them here at offsets that are right on exactly one of the two.
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
                                     // `filename[1024]` and later releases kept rearranging what replaced it. It and `start_time`
                                     // are the only fields beyond this point the app reads, so they are read at OFF_FMT_DURATION
                                     // and OFF_FMT_START_TIME rather than declared at a position that would be quietly wrong
                                     // after a version bump.
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
    // 2026-08-28.** The consequence was latent rather than live — `rect_to_indexed` reads only
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

// ---- bundled FFmpeg entry points, resolved by absolute path beside the app ----
// Each table is all-or-nothing. The loader opens avutil, avcodec, then avformat in
// dependency order; swscale is optional for the capture encoder. `boot` checks the
// bundled majors before any caller may use the modelled layouts above.
plx_base::dynlib! {
    pub avformat: [
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
plx_base::dynlib! {
    pub avcodec: [
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
    // Audio decode, send/receive API: playback speed (`SpeedDecoder`) and lossless → PCM (`ff_pcm`).
    fn avcodec_send_packet(ctx: *mut AVCodecContext, pkt: *const AVPacket) -> c_int;
    fn avcodec_receive_frame(ctx: *mut AVCodecContext, frame: *mut c_void) -> c_int;
    // A seek: drop what the decoder carries from before it. Public libavcodec API since the
    // 1.x series and in the bundled `libavcodec-plx` (the library `dynlib!` opens is OUR build,
    // not the television's): an export of both the ARM and the host build, checked by hand with
    // `readelf --dyn-syms`. No CI step compares a `dynlib!` table with the bundled library's
    // exports (`ci/check-elf.sh` reads only the app binary): a name the build did not export would
    // leave this whole table `Incomplete` at start-up and refuse playback on every firmware.
    fn avcodec_flush_buffers(ctx: *mut AVCodecContext);
    fn avcodec_find_decoder_by_name(name: *const c_char) -> *const AVCodec;
}}
plx_base::dynlib! {
    // avformat_version lives in libavformat, not libavutil — but it was declared here and the
    // loader resolves by symbol, not by header, so it must move to the library that defines it or
    // the whole avutil table reports Incomplete on every device.
    pub avutil: [
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
    fn av_frame_unref(frame: *mut c_void);
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
plx_base::dynlib! {
    pub swscale: [
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

// ---- bundled FFmpeg 9.0 constants ----
pub const AVMEDIA_TYPE_VIDEO: c_int = 0;
pub const AVMEDIA_TYPE_AUDIO: c_int = 1;
pub const AVMEDIA_TYPE_SUBTITLE: c_int = 3;
// Proven against the bundled headers by ci/ffabi-assert.c.
pub const AVMEDIA_TYPE_ATTACHMENT: c_int = 4;
// The codec ids the app compares against are AAC, H264, HEVC, VP9 and AV1. Their values shift between
// FFmpeg majors, so each is asserted against the bundled headers in ci/ffabi-assert.c.
// AV1 needs no FFmpeg parser or bsf: matroska (V_AV1) and mov (av01) demux it without one, and
// with the av1 parser absent the packets arrive raw. `av1_packet_to_tu` frames the OBUs itself.
// Enabling the parser would pull in cbs_av1 for nothing.
pub const AV_CODEC_ID_AAC: c_int = 0x15002;
pub const AV_CODEC_ID_H264: c_int = 27;
// FF_API_XVMC and FF_API_VOXWARE both died before FFmpeg 6, which is why these differ from the
// values the n3.3 televisions use (28 / 174 / 0x15029).
pub const AV_CODEC_ID_HEVC: c_int = 172;
pub const AV_CODEC_ID_VP9: c_int = 166;
pub const AV_CODEC_ID_AV1: c_int = 222;
pub const AV_PKT_FLAG_KEY: c_int = 1;
pub const AVSEEK_FLAG_BACKWARD: c_int = 1;
pub const AVERROR_EOF: c_int = -541478725;
/// `AVERROR(EAGAIN)`: FFmpeg negates the HOST libc's errno, which is 11 on Linux (the television)
/// but 35 on macOS (the simulator), so it cannot be a literal.
pub const AVERROR_EAGAIN: c_int = -(libc::EAGAIN as c_int);
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
/// `AV_PKT_DATA_MASTERING_DISPLAY_METADATA` (an `AVMasteringDisplayMetadata`, 88 bytes) and
/// `AV_PKT_DATA_CONTENT_LIGHT_LEVEL` (an `AVContentLightMetadata`, 8 bytes): 20 and 22 in FFmpeg 9.0,
/// asserted in `ci/ffabi-assert.c` for the same reason as the Dolby Vision tag. The two structs'
/// member offsets are hard-coded in `plx_play_feed::vp9_hdr::{parse_mastering, parse_cll}` and pinned by
/// the same file; a bump that moves them has to change both.
pub const AV_PKT_DATA_MASTERING_DISPLAY_METADATA: c_int = 20;
pub const AV_PKT_DATA_CONTENT_LIGHT_LEVEL: c_int = 22;

// ---- AVStream field accessors, at the constants above. Read by offset rather than modelled:
// the struct is large, mostly internal, and only three fields are wanted. ----
#[inline]
pub unsafe fn stream_codecpar(s: *mut AVStream) -> *mut AVCodecParameters {
    *((s as *const u8).add(OFF_STREAM_CODECPAR) as *const *mut AVCodecParameters)
}
#[inline]
pub unsafe fn stream_time_base(s: *mut AVStream) -> AVRational {
    *((s as *const u8).add(OFF_STREAM_TIME_BASE) as *const AVRational)
}

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
pub const OFF_FRAME_DATA: usize = 0; // u8*[8] — the only one that does not move
#[cfg(target_pointer_width = "32")]
pub const OFF_FRAME_LINESIZE: usize = 32; // c_int[8]
#[cfg(target_pointer_width = "32")]
pub const OFF_FRAME_WIDTH: usize = 68;
#[cfg(target_pointer_width = "32")]
pub const OFF_FRAME_HEIGHT: usize = 72;
#[cfg(target_pointer_width = "32")]
pub const OFF_FRAME_FORMAT: usize = 80;
#[cfg(target_pointer_width = "32")]
pub const OFF_FRAME_PTS: usize = 96;
// 64-bit: `data` is eight POINTERS rather than eight 32-bit ones, so `linesize` doubles and
// everything after it follows. `pts` needs no pad here — it is already 8-aligned — which is the
// ARM EABI quirk described above seen from the side where it does not bite.
#[cfg(target_pointer_width = "64")]
pub const OFF_FRAME_LINESIZE: usize = 64;
#[cfg(target_pointer_width = "64")]
pub const OFF_FRAME_WIDTH: usize = 104;
#[cfg(target_pointer_width = "64")]
pub const OFF_FRAME_HEIGHT: usize = 108;
#[cfg(target_pointer_width = "64")]
pub const OFF_FRAME_FORMAT: usize = 116;
#[cfg(target_pointer_width = "64")]
pub const OFF_FRAME_PTS: usize = 136;
// The AUDIO half of AVFrame, read by `SpeedDecoder` and by `ff_pcm::PcmDecoder` (FFmpeg 9, the bundled build). `ch_layout` is an
// embedded AVChannelLayout whose `nb_channels` sits 4 bytes in (after the `order` enum) on both
// widths. Every one is asserted in `ci/ffabi-assert.c` against the bundled headers.
#[cfg(target_pointer_width = "32")]
pub const OFF_AFRAME_EXTENDED_DATA: usize = 64; // u8**
#[cfg(target_pointer_width = "32")]
pub const OFF_AFRAME_NB_SAMPLES: usize = 76;
#[cfg(target_pointer_width = "32")]
pub const OFF_AFRAME_SAMPLE_RATE: usize = 132;
#[cfg(target_pointer_width = "32")]
pub const OFF_AFRAME_NB_CHANNELS: usize = 256 + 4;
// The layout's `order` (offset 0 of the AVChannelLayout) and its `u.mask` (8-aligned after
// `nb_channels`); `mask` is only meaningful when `order` is native (`CH_ORDER_NATIVE`).
#[cfg(target_pointer_width = "32")]
pub const OFF_AFRAME_CH_ORDER: usize = 256;
#[cfg(target_pointer_width = "32")]
pub const OFF_AFRAME_CH_MASK: usize = 264;
#[cfg(target_pointer_width = "64")]
pub const OFF_AFRAME_EXTENDED_DATA: usize = 96;
#[cfg(target_pointer_width = "64")]
pub const OFF_AFRAME_NB_SAMPLES: usize = 112;
#[cfg(target_pointer_width = "64")]
pub const OFF_AFRAME_SAMPLE_RATE: usize = 180;
#[cfg(target_pointer_width = "64")]
pub const OFF_AFRAME_NB_CHANNELS: usize = 384 + 4;
#[cfg(target_pointer_width = "64")]
pub const OFF_AFRAME_CH_ORDER: usize = 384;
#[cfg(target_pointer_width = "64")]
pub const OFF_AFRAME_CH_MASK: usize = 392;

/// `AV_CHANNEL_ORDER_NATIVE`: the layout is a bit mask, channels in ascending bit order
/// (`ci/ffabi-assert.c` proves the value).
const CH_ORDER_NATIVE: c_int = 1;

/// The native channel mask of a decoded audio frame (`frame` points at the `AVFrame`), or 0 when the
/// layout is not a mask (the union then holds a pointer, or nothing): the caller mixes by count.
/// The ONE reader of `OFF_AFRAME_CH_ORDER` / `OFF_AFRAME_CH_MASK`, for the lossy app decode
/// (`ff_pcm`) and the speed decoder alike.
pub unsafe fn frame_channel_mask(frame: *const u8) -> u64 {
    if *(frame.add(OFF_AFRAME_CH_ORDER) as *const c_int) == CH_ORDER_NATIVE {
        std::ptr::read_unaligned(frame.add(OFF_AFRAME_CH_MASK) as *const u64)
    } else {
        0
    }
}

/// AVIOContext leading fields used by the existing manual buffer/context free path.
/// `buffer` follows `av_class`; this layout is asserted for the bundled build.
#[repr(C)]
struct AVIOCtxHead {
    av_class: *const c_void,
    buffer: *mut u8,
}

pub unsafe fn free_ptr(p: *mut c_void) {
    let mut q = p;
    av_freep(&mut q as *mut *mut c_void as *mut c_void);
}

/// Free a custom AVIOContext (buffer + context). `avformat_close_input` does not free
/// a caller-set `pb`.
pub unsafe fn free_avio(avio: *mut AVIOContext) {
    if avio.is_null() {
        return;
    }
    let head = avio as *mut AVIOCtxHead;
    av_freep(std::ptr::addr_of_mut!((*head).buffer) as *mut c_void); // frees + NULLs avio->buffer
    let mut p = avio;
    av_freep(&mut p as *mut *mut AVIOContext as *mut c_void); // frees + NULLs the context
}


static REGISTER: Once = Once::new();
/// One-time `avformat_network_init`. Was also the home of `av_register_all`, which FFmpeg 4.0
/// made a no-op and 5.0 deleted — registration is automatic now, and the bundled build is 9.0.
pub fn ensure_registered() {
    REGISTER.call_once(|| unsafe {
        avformat_network_init();
    });
}

fn ver(v: c_uint) -> String {
    format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff)
}

/// Resolve the four bundled FFmpeg libraries by absolute path. `false` means the
/// payload is unavailable; no wrapper may be called, not even `avformat_version`.
///
/// Kept separate from the version check that follows it because the two failures are different
/// facts about the television and deserve different log lines: "there is no libavformat here at
/// all" versus "there is one and it is the wrong major".
fn load_libraries() -> bool {
    // The bundled FFmpeg lives beside the binary, which is on no library search path — so it is
    // opened by ABSOLUTE PATH. That is not a convenience: webOS 11.2.0 ships FFmpeg 6 itself, and
    // a bare SONAME there could open the television's copy instead of ours.
    let dir = Some(plx_base::paths::app_dir());

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
            plx_base::dynlib::Loaded::Ok(soname) => {
                plx_base::eventlog::log(&format!("ff: bound {what} -> {soname}"))
            }
            plx_base::dynlib::Loaded::NoLibrary => {
                ok = false;
                plx_base::eventlog::log(&format!(
                    "ff: {what} is MISSING from the app directory — the bundled FFmpeg did not \
                     deploy. Playback will refuse; reinstall the package."
                ));
            }
            plx_base::dynlib::Loaded::Incomplete(soname, n) => {
                ok = false;
                plx_base::eventlog::log(&format!(
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
        plx_base::dynlib::Loaded::Ok(soname) => {
            SWS_OK.store(true, Ordering::Relaxed);
            plx_base::eventlog::log(&format!("ff: bound swscale -> {soname}"));
        }
        _ => {
            plx_base::eventlog::log("ff: no swscale (expected in a RELEASE build) — dev capture JPEG/MPEG1 off")
        }
    }
    ok
}

/// Can the bundled FFmpeg decode the route's audio codec `codec` (`route::stream_acodec`'s
/// lower-case name) for a playback-speed session? Asked at Load, because a speed session declares
/// PCM and so must decode — a codec missing from `ci/build-ffmpeg.sh`'s list means 1x instead.
pub fn speed_decoder_available(codec: &str) -> bool {
    #[cfg(any(test, feature = "test-support"))]
    if FORCE_SPEED_DECODER.load(Ordering::Relaxed) {
        return true;
    }
    if !abi_ok() {
        return false;
    }
    let name = match codec {
        "dts" | "dca" => "dca",
        other => other,
    };
    let Ok(c) = std::ffi::CString::new(name) else { return false };
    unsafe { !avcodec_find_decoder_by_name(c.as_ptr()).is_null() }
}

/// Test-only: answer [`speed_decoder_available`] as if the bundled FFmpeg had every decoder, so a
/// host test (which loads no FFmpeg) can reach the speed paths.
#[cfg(any(test, feature = "test-support"))]
static FORCE_SPEED_DECODER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(any(test, feature = "test-support"))]
pub fn force_speed_decoder_for_test(on: bool) {
    FORCE_SPEED_DECODER.store(on, Ordering::Relaxed);
}

/// Did the bundled FFmpeg load and match the ABI table? The diagnostics read-out's first question
/// about the demuxer: false means every playback will refuse before it opens a socket, which from
/// the outside looks identical to a stall.
pub fn abi_ok() -> bool {
    ABI_OK.load(std::sync::atomic::Ordering::Relaxed)
}

/// Preserve the previous gate across a nested host FFmpeg fixture.
#[cfg(any(test, feature = "test-support"))]
pub fn swap_abi_for_test(on: bool) -> bool {
    ABI_OK.swap(on, Ordering::Relaxed)
}

/// The three FFmpeg majors actually bound, for the read-out's build line. Zeroes before [`boot`]
/// has run or when the libraries did not load.
pub fn majors() -> (u32, u32, u32) {
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

/// Check the bundled ABI once after process initialization, before codec work begins.
pub fn boot() {
    if !load_libraries() {
        ABI_OK.store(false, Ordering::Relaxed);
        plx_base::eventlog::log("ff: FFmpeg unavailable — the app runs, playback will refuse");
        return;
    }
    unsafe {
        let (fmt, cod, utl) = (avformat_version(), avcodec_version(), avutil_version());
        plx_base::eventlog::log(&format!(
            "ff: avformat={} avcodec={} avutil={}", ver(fmt), ver(cod), ver(utl)
        ));
        let bad = (fmt >> 16, cod >> 16, utl >> 16) != (63, 63, 61);
        ABI_OK.store(!bad, Ordering::Relaxed);
        if bad {
            plx_base::eventlog::log(
                "ff: BUNDLED FFmpeg is not the one this build expects (want avformat 63 / \
                 avcodec 63 / avutil 61) — the app directory holds a stale or foreign \
                 libav*-plx; refusing to demux",
            );
        }
    }
}

pub fn sws_ok() -> bool { SWS_OK.load(Ordering::Relaxed) }
