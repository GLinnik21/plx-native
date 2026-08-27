/* Compile-time proof that rust-modules/src/ff.rs's FFmpeg ABI constants match the FFmpeg the app
 * SHIPS — compiled against the very headers those libraries were built from.
 *
 * WHAT CHANGED. This file used to check offsets against the TELEVISION's FFmpeg, re-derived from
 * upstream headers at whatever version the firmware reported (55 / 57 / 58 / 59 / 60 across webOS
 * 2 to 11). That worked, because FFmpeg's public headers carry no `#if CONFIG_*` so layout is a
 * function of the version macros alone — but it could only ever be an inference, and it said
 * nothing at all about whether the demuxers, parsers and bitstream filters we need were compiled
 * into that build. Those live in a registry, as data; no symbol table can see them.
 *
 * The app bundles its own FFmpeg now (ci/build-ffmpeg.sh), so both questions are answered by the
 * build: these assertions run against vendor/ffmpeg-prefix/include, which is installed by the same
 * invocation that produced the .so files in the package.
 *
 * COMPILED, NEVER LINKED. It contains no code — only assertions — and is a Makefile prerequisite
 * of the Rust staticlib, so no binary is produced if a constant is wrong.
 *
 * If the bundled version is ever bumped: `tools/ffabi-dump.sh` prints the new table, paste it into
 * ff.rs, and this file fails on every constant that moved until you do.
 */
#include <stddef.h>
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavcodec/bsf.h>
#include <libavutil/frame.h>
#include <libavutil/channel_layout.h>
#include <libavutil/dovi_meta.h>
#include <libavutil/dict.h>

#define SAME(expr, want, what) _Static_assert((expr) == (want), what)

/* **Two tables, and the axis is POINTER WIDTH — not FFmpeg version.**
 *
 * The 32-bit arm is the television. The 64-bit arm is the desktop simulator (`make sim`), which
 * runs this same Rust against a HOST build of the same FFmpeg 9.0 from the same
 * `ci/build-ffmpeg.sh` component list — see that script's HOST=1 note for why one script. Every
 * number that differs below differs because a pointer got wider or an int64 moved to keep its
 * alignment, and each is compiled against ITS OWN build's headers, so a wrong constant is a
 * compile error on the platform it is wrong for and nowhere else.
 *
 * This is not the old runtime major-selected table coming back. That one picked between FFmpeg
 * VERSIONS at run time on a device whose libraries we could not inspect; this picks between two
 * ABIs of ONE version at COMPILE time, on evidence the compiler has in front of it.
 *
 * **It has already earned its keep.** `AVSubtitleRect` was modelled in ff.rs with `flags` last
 * when the header puts it before `type` — latent, because nothing reads those four fields, and
 * invisible to the 32-bit assertions because they never asserted `flags` at all. Deriving the
 * table a second time at a different width is what surfaced it: on the host the model's `flags`
 * landed at offset 96 of a 96-byte struct. All four are pinned below now, on both widths. */
#if __SIZEOF_POINTER__ == 4
#define SAME32(expr, want, what) SAME(expr, want, what)
#define SAME64(expr, want, what)
#elif __SIZEOF_POINTER__ == 8
#define SAME32(expr, want, what)
#define SAME64(expr, want, what) SAME(expr, want, what)
#else
#error "ff.rs has an ABI table for 32- and 64-bit pointers and no other width"
#endif

/* --- the version ff.rs's constants describe, and the version boot() demands at runtime. --- */
SAME(LIBAVFORMAT_VERSION_MAJOR, 63, "bundled libavformat is not 63 — ff.rs's table is for 9.0");
SAME(LIBAVCODEC_VERSION_MAJOR, 63, "bundled libavcodec is not 63");
SAME(LIBAVUTIL_VERSION_MAJOR, 61, "bundled libavutil is not 61");

/* --- AVStream: read by offset. NB index is +4, not +0: FFmpeg 5.0 put av_class first. --- */
SAME32(offsetof(AVStream, index), 4, "OFF_STREAM_INDEX != 4");
SAME64(offsetof(AVStream, index), 8, "OFF_STREAM_INDEX != 4 [host]");
SAME32(offsetof(AVStream, codecpar), 12, "OFF_STREAM_CODECPAR != 12");
SAME64(offsetof(AVStream, codecpar), 16, "OFF_STREAM_CODECPAR != 12 [host]");
SAME32(offsetof(AVStream, time_base), 20, "OFF_STREAM_TIME_BASE != 20");
SAME64(offsetof(AVStream, time_base), 32, "OFF_STREAM_TIME_BASE != 20 [host]");
/* The container's per-track tags — where a track's NAME is, and the only place it is for an MP4
   (PMS sends Stream.title for Matroska and nothing at all for MP4; see ff.rs's stream_name). The
   int64 start_time/duration/nb_frames ahead of it are 8-ALIGNED on ARM EABI, so there is a 4-byte
   pad after time_base that a 64-bit reading of the struct does not show — the same trap
   AVFrame.pts carries below. */
SAME32(offsetof(AVStream, metadata), 72, "OFF_STREAM_METADATA != 72");
SAME64(offsetof(AVStream, metadata), 80, "OFF_STREAM_METADATA != 72 [host]");
SAME(offsetof(AVDictionaryEntry, key), 0, "AVDictionaryEntry.key moved");
SAME32(offsetof(AVDictionaryEntry, value), 4, "AVDictionaryEntry.value moved");
SAME64(offsetof(AVDictionaryEntry, value), 8, "AVDictionaryEntry.value moved [host]");
SAME32(sizeof(AVDictionaryEntry), 8, "AVDictionaryEntry is not two 32-bit pointers");
SAME64(sizeof(AVDictionaryEntry), 16, "AVDictionaryEntry is not two 32-bit pointers [host]");

/* --- AVFormatContext: modelled through `duration`. FFmpeg 5.0 deleted filename[1024], which is
       why duration is at +48 here and +1064 on the n3.3 televisions ship. --- */
SAME32(offsetof(AVFormatContext, pb), 16, "AVFormatContext.pb moved");
SAME64(offsetof(AVFormatContext, pb), 32, "AVFormatContext.pb moved [host]");
SAME32(offsetof(AVFormatContext, nb_streams), 24, "AVFormatContext.nb_streams moved");
SAME64(offsetof(AVFormatContext, nb_streams), 44, "AVFormatContext.nb_streams moved [host]");
SAME32(offsetof(AVFormatContext, streams), 28, "AVFormatContext.streams moved");
SAME64(offsetof(AVFormatContext, streams), 48, "AVFormatContext.streams moved [host]");
SAME32(offsetof(AVFormatContext, duration), 64, "OFF_FMT_DURATION != 64");
SAME64(offsetof(AVFormatContext, duration), 104, "OFF_FMT_DURATION != 64 [host]");

/* --- AVFrame: poked directly by the encode path. --- */
SAME(offsetof(AVFrame, data), 0, "OFF_FRAME_DATA != 0");
SAME32(offsetof(AVFrame, linesize), 32, "OFF_FRAME_LINESIZE != 32");
SAME64(offsetof(AVFrame, linesize), 64, "OFF_FRAME_LINESIZE != 32 [host]");
SAME32(offsetof(AVFrame, width), 68, "OFF_FRAME_WIDTH != 68");
SAME64(offsetof(AVFrame, width), 104, "OFF_FRAME_WIDTH != 68 [host]");
SAME32(offsetof(AVFrame, height), 72, "OFF_FRAME_HEIGHT != 72");
SAME64(offsetof(AVFrame, height), 108, "OFF_FRAME_HEIGHT != 72 [host]");
SAME32(offsetof(AVFrame, format), 80, "OFF_FRAME_FORMAT != 80");
SAME64(offsetof(AVFrame, format), 116, "OFF_FRAME_FORMAT != 80 [host]");
/* +96 on FFmpeg 9, +104 on 6 and on the n3.3 the TVs ship — AVFrame lost fields ahead of it.
   Still 8-aligned by an ARM EABI pad, which is the classic AVFrame-on-ARM trap and the one most
   likely to be got wrong by reading a struct definition on a 64-bit machine. This assertion is
   the reason the FFmpeg 9 bump did not ship a wrong PTS: it fired here, on a desk. */
SAME32(offsetof(AVFrame, pts), 96, "OFF_FRAME_PTS != 96 (ARM EABI int64 alignment)");
SAME64(offsetof(AVFrame, pts), 136, "OFF_FRAME_PTS != 96 (ARM EABI int64 alignment) [host]");

/* --- AVPacket: modelled field-by-field in ff.rs. Never allocated by us (av_packet_alloc does),
       but a short model would be a silent overread. sizeof went 72 -> 80 at FFmpeg 5.0:
       convergence_duration left with FF_API_CONVERGENCE_DURATION, opaque/opaque_ref/time_base
       arrived. --- */
SAME32(sizeof(AVPacket), 80, "sizeof(AVPacket) != 80");
SAME64(sizeof(AVPacket), 104, "sizeof(AVPacket) != 80 [host]");
SAME(offsetof(AVPacket, pts), 8, "AVPacket.pts moved");
SAME(offsetof(AVPacket, dts), 16, "AVPacket.dts moved");
SAME(offsetof(AVPacket, data), 24, "AVPacket.data moved");
SAME32(offsetof(AVPacket, size), 28, "AVPacket.size moved");
SAME64(offsetof(AVPacket, size), 32, "AVPacket.size moved [host]");
SAME32(offsetof(AVPacket, stream_index), 32, "AVPacket.stream_index moved");
SAME64(offsetof(AVPacket, stream_index), 36, "AVPacket.stream_index moved [host]");
SAME32(offsetof(AVPacket, flags), 36, "AVPacket.flags moved");
SAME64(offsetof(AVPacket, flags), 40, "AVPacket.flags moved [host]");
SAME32(offsetof(AVPacket, duration), 48, "AVPacket.duration moved");
SAME64(offsetof(AVPacket, duration), 64, "AVPacket.duration moved [host]");
SAME32(offsetof(AVPacket, time_base), 72, "AVPacket.time_base moved");
SAME64(offsetof(AVPacket, time_base), 96, "AVPacket.time_base moved [host]");

/* --- AVCodecParameters: ff.rs models a read-only PREFIX and must NEVER allocate one. sizeof is
       168 against the bundled FFmpeg 9.0 headers — the number the assertion below demands, and the
       one that compiles — and avcodec_parameters_from_context memsets the full size, so a stack
       copy sized by our model is a stack smash. venc allocates through the library.
       (This prose said 176 while the assertion three lines down said 168. This file is the
       authority on the ABI table, so a wrong figure in its own comment is the one place it gets
       believed: the next person bumping FFmpeg reads it and edits ff.rs's model to match.) --- */
SAME32(sizeof(AVCodecParameters), 168, "sizeof(AVCodecParameters) != 168 — do not stack-allocate it");
SAME64(sizeof(AVCodecParameters), 184, "sizeof(AVCodecParameters) != 168 — do not stack-allocate it [host]");
SAME(offsetof(AVCodecParameters, codec_type), 0, "AVCodecParameters.codec_type moved");
SAME(offsetof(AVCodecParameters, codec_id), 4, "AVCodecParameters.codec_id moved");
SAME32(offsetof(AVCodecParameters, extradata), 12, "AVCodecParameters.extradata moved");
SAME64(offsetof(AVCodecParameters, extradata), 16, "AVCodecParameters.extradata moved [host]");
SAME32(offsetof(AVCodecParameters, extradata_size), 16, "AVCodecParameters.extradata_size moved");
SAME64(offsetof(AVCodecParameters, extradata_size), 24, "AVCodecParameters.extradata_size moved [host]");
SAME32(offsetof(AVCodecParameters, format), 28, "AVCodecParameters.format moved");
SAME64(offsetof(AVCodecParameters, format), 44, "AVCodecParameters.format moved [host]");
SAME32(offsetof(AVCodecParameters, width), 56, "AVCodecParameters.width moved");
SAME64(offsetof(AVCodecParameters, width), 72, "AVCodecParameters.width moved [host]");
SAME32(offsetof(AVCodecParameters, height), 60, "AVCodecParameters.height moved");
SAME64(offsetof(AVCodecParameters, height), 76, "AVCodecParameters.height moved [host]");
/* The stream-level side-data list, walked for the Dolby Vision configuration record, and the
   three colour fields logged beside it. All five were modelled in ff.rs long before anything
   read them, which is exactly why they are asserted now rather than then. */
SAME32(offsetof(AVCodecParameters, coded_side_data), 20, "AVCodecParameters.coded_side_data moved");
SAME64(offsetof(AVCodecParameters, coded_side_data), 32, "AVCodecParameters.coded_side_data moved [host]");
SAME32(offsetof(AVCodecParameters, nb_coded_side_data), 24, "AVCodecParameters.nb_coded_side_data moved");
SAME64(offsetof(AVCodecParameters, nb_coded_side_data), 40, "AVCodecParameters.nb_coded_side_data moved [host]");
SAME32(offsetof(AVCodecParameters, color_primaries), 88, "AVCodecParameters.color_primaries moved");
SAME64(offsetof(AVCodecParameters, color_primaries), 104, "AVCodecParameters.color_primaries moved [host]");
SAME32(offsetof(AVCodecParameters, color_trc), 92, "AVCodecParameters.color_trc moved");
SAME64(offsetof(AVCodecParameters, color_trc), 108, "AVCodecParameters.color_trc moved [host]");
SAME32(offsetof(AVCodecParameters, color_space), 96, "AVCodecParameters.color_space moved");
SAME64(offsetof(AVCodecParameters, color_space), 112, "AVCodecParameters.color_space moved [host]");
SAME32(offsetof(AVCodecParameters, sample_rate), 136, "AVCodecParameters.sample_rate moved");
SAME64(offsetof(AVCodecParameters, sample_rate), 152, "AVCodecParameters.sample_rate moved [host]");
/* FFmpeg 7 deleted the deprecated `channels` int; nb_channels inside ch_layout replaced it, and
   reading the old field on a hand-written model is a silent read of whatever now sits there. */
SAME32(offsetof(AVCodecParameters, ch_layout), 112, "AVCodecParameters.ch_layout moved");
SAME64(offsetof(AVCodecParameters, ch_layout), 128, "AVCodecParameters.ch_layout moved [host]");
SAME(offsetof(AVChannelLayout, nb_channels), 4, "AVChannelLayout.nb_channels moved");

/* --- AVPacketSideData: one entry of the list above. `size` is a size_t, so the whole struct is
       12 bytes on 32-bit ARM and 24 on the 64-bit host — ff.rs spells that field `usize` so the
       model is right on both. --- */
SAME32(sizeof(AVPacketSideData), 12, "sizeof(AVPacketSideData) != 12");
SAME64(sizeof(AVPacketSideData), 24, "sizeof(AVPacketSideData) != 12 [host]");
SAME(offsetof(AVPacketSideData, data), 0, "AVPacketSideData.data moved");
SAME32(offsetof(AVPacketSideData, size), 4, "AVPacketSideData.size moved");
SAME64(offsetof(AVPacketSideData, size), 8, "AVPacketSideData.size moved [host]");
SAME32(offsetof(AVPacketSideData, type), 8, "AVPacketSideData.type moved");
SAME64(offsetof(AVPacketSideData, type), 16, "AVPacketSideData.type moved [host]");

/* --- AVDOVIDecoderConfigurationRecord: nine plain uint8_t, no padding on any target. Asserted
       anyway because the header says its size "is not a part of the public ABI", and because a
       field inserted in the middle would silently renumber every DV profile the app logs and
       gates on. AV_PKT_DATA_DOVI_CONF is a sequential enum member, so it moves the same way. --- */
SAME(sizeof(AVDOVIDecoderConfigurationRecord), 9, "sizeof(AVDOVIDecoderConfigurationRecord) != 9");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, dv_version_major), 0, "DOVI dv_version_major moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, dv_version_minor), 1, "DOVI dv_version_minor moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, dv_profile), 2, "DOVI dv_profile moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, dv_level), 3, "DOVI dv_level moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, rpu_present_flag), 4, "DOVI rpu_present_flag moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, el_present_flag), 5, "DOVI el_present_flag moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, bl_present_flag), 6, "DOVI bl_present_flag moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, dv_bl_signal_compatibility_id), 7, "DOVI bl_signal_compatibility_id moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, dv_md_compression), 8, "DOVI dv_md_compression moved");
SAME(AV_PKT_DATA_DOVI_CONF, 29, "AV_PKT_DATA_DOVI_CONF != 29 — the side-data enum shifted");

/* --- AVSubtitle: avcodec_decode_subtitle2 writes into ff.rs's stack copy, so this size IS
       load-bearing. Unchanged from n3.3. --- */
SAME(sizeof(AVSubtitle), 32, "sizeof(AVSubtitle) != 32 — ff.rs stack-allocates this one");
SAME(offsetof(AVSubtitle, num_rects), 12, "AVSubtitle.num_rects moved");
SAME(offsetof(AVSubtitle, rects), 16, "AVSubtitle.rects moved");
SAME(offsetof(AVSubtitle, pts), 24, "AVSubtitle.pts moved");

/* --- AVSubtitleRect: sizeof 132 -> 68, because FFmpeg 5.0 deleted the embedded deprecated
       AVPicture that used to push `data` all the way out to +84. --- */
SAME32(sizeof(AVSubtitleRect), 68, "sizeof(AVSubtitleRect) != 68");
SAME64(sizeof(AVSubtitleRect), 96, "sizeof(AVSubtitleRect) != 68 [host]");
SAME(offsetof(AVSubtitleRect, w), 8, "AVSubtitleRect.w moved");
SAME(offsetof(AVSubtitleRect, h), 12, "AVSubtitleRect.h moved");
SAME32(offsetof(AVSubtitleRect, data), 20, "AVSubtitleRect.data != 20 (the AVPicture is gone)");
SAME64(offsetof(AVSubtitleRect, data), 24, "AVSubtitleRect.data != 20 (the AVPicture is gone) [host]");
SAME32(offsetof(AVSubtitleRect, linesize), 36, "AVSubtitleRect.linesize moved");
SAME64(offsetof(AVSubtitleRect, linesize), 56, "AVSubtitleRect.linesize moved [host]");
/* `flags` sits BETWEEN linesize and type, and ff.rs modelled it last until 2026-08-28. Nothing
   reads these four, so it was latent — and it was invisible here because `flags`, `text` and
   `ass` were never asserted. They are now: an ORDER is only pinned by asserting every member of
   it, and the three that were missing are exactly the three that were wrong. */
SAME32(offsetof(AVSubtitleRect, flags), 52, "AVSubtitleRect.flags moved");
SAME64(offsetof(AVSubtitleRect, flags), 72, "AVSubtitleRect.flags moved [host]");
SAME32(offsetof(AVSubtitleRect, type), 56, "AVSubtitleRect.type moved");
SAME64(offsetof(AVSubtitleRect, type), 76, "AVSubtitleRect.type moved [host]");
SAME32(offsetof(AVSubtitleRect, text), 60, "AVSubtitleRect.text moved");
SAME64(offsetof(AVSubtitleRect, text), 80, "AVSubtitleRect.text moved [host]");
SAME32(offsetof(AVSubtitleRect, ass), 64, "AVSubtitleRect.ass moved");
SAME64(offsetof(AVSubtitleRect, ass), 88, "AVSubtitleRect.ass moved [host]");

/* --- AVBSFContext: `AVBSFInternal *internal` was removed in 5.0, pulling the rest up 4 bytes. --- */
SAME32(offsetof(AVBSFContext, par_in), 12, "AVBSFContext.par_in moved");
SAME64(offsetof(AVBSFContext, par_in), 24, "AVBSFContext.par_in moved [host]");
SAME32(offsetof(AVBSFContext, time_base_in), 20, "AVBSFContext.time_base_in moved");
SAME64(offsetof(AVBSFContext, time_base_in), 40, "AVBSFContext.time_base_in moved [host]");

/* --- enum constants ff.rs hardcodes. H264/HEVC/E-AC3 all shifted down when FF_API_XVMC and
       FF_API_VOXWARE were removed; the n3.3 values are 28 / 174 / 0x15029. --- */
SAME(AV_CODEC_ID_H264, 27, "AV_CODEC_ID_H264 != 27");
SAME(AV_CODEC_ID_HEVC, 172, "AV_CODEC_ID_HEVC != 172 (173 on FFmpeg 6 — the enum shifts again)");
SAME(AV_CODEC_ID_AAC, 0x15002, "AV_CODEC_ID_AAC moved");
SAME(AV_CODEC_ID_AC3, 0x15003, "AV_CODEC_ID_AC3 moved");
SAME(AV_CODEC_ID_EAC3, 0x15028, "AV_CODEC_ID_EAC3 != 0x15028 (0x15029 names ATRAC3P here)");
SAME(SUBTITLE_BITMAP, 1, "SUBTITLE_BITMAP != 1 (NONE is 0)");
SAME(AVMEDIA_TYPE_VIDEO, 0, "AVMEDIA_TYPE_VIDEO != 0");
SAME(AVMEDIA_TYPE_AUDIO, 1, "AVMEDIA_TYPE_AUDIO != 1");
SAME(AVMEDIA_TYPE_SUBTITLE, 3, "AVMEDIA_TYPE_SUBTITLE != 3");
