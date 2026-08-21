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

#define SAME(expr, want, what) _Static_assert((expr) == (want), what)

/* --- the version ff.rs's constants describe, and the version boot() demands at runtime. --- */
SAME(LIBAVFORMAT_VERSION_MAJOR, 63, "bundled libavformat is not 63 — ff.rs's table is for 9.0");
SAME(LIBAVCODEC_VERSION_MAJOR, 63, "bundled libavcodec is not 63");
SAME(LIBAVUTIL_VERSION_MAJOR, 61, "bundled libavutil is not 61");

/* --- AVStream: read by offset. NB index is +4, not +0: FFmpeg 5.0 put av_class first. --- */
SAME(offsetof(AVStream, index), 4, "OFF_STREAM_INDEX != 4");
SAME(offsetof(AVStream, codecpar), 12, "OFF_STREAM_CODECPAR != 12");
SAME(offsetof(AVStream, time_base), 20, "OFF_STREAM_TIME_BASE != 20");

/* --- AVFormatContext: modelled through `duration`. FFmpeg 5.0 deleted filename[1024], which is
       why duration is at +48 here and +1064 on the n3.3 televisions ship. --- */
SAME(offsetof(AVFormatContext, pb), 16, "AVFormatContext.pb moved");
SAME(offsetof(AVFormatContext, nb_streams), 24, "AVFormatContext.nb_streams moved");
SAME(offsetof(AVFormatContext, streams), 28, "AVFormatContext.streams moved");
SAME(offsetof(AVFormatContext, duration), 64, "OFF_FMT_DURATION != 64");

/* --- AVFrame: poked directly by the encode path. --- */
SAME(offsetof(AVFrame, data), 0, "OFF_FRAME_DATA != 0");
SAME(offsetof(AVFrame, linesize), 32, "OFF_FRAME_LINESIZE != 32");
SAME(offsetof(AVFrame, width), 68, "OFF_FRAME_WIDTH != 68");
SAME(offsetof(AVFrame, height), 72, "OFF_FRAME_HEIGHT != 72");
SAME(offsetof(AVFrame, format), 80, "OFF_FRAME_FORMAT != 80");
/* +96 on FFmpeg 9, +104 on 6 and on the n3.3 the TVs ship — AVFrame lost fields ahead of it.
   Still 8-aligned by an ARM EABI pad, which is the classic AVFrame-on-ARM trap and the one most
   likely to be got wrong by reading a struct definition on a 64-bit machine. This assertion is
   the reason the FFmpeg 9 bump did not ship a wrong PTS: it fired here, on a desk. */
SAME(offsetof(AVFrame, pts), 96, "OFF_FRAME_PTS != 96 (ARM EABI int64 alignment)");

/* --- AVPacket: modelled field-by-field in ff.rs. Never allocated by us (av_packet_alloc does),
       but a short model would be a silent overread. sizeof went 72 -> 80 at FFmpeg 5.0:
       convergence_duration left with FF_API_CONVERGENCE_DURATION, opaque/opaque_ref/time_base
       arrived. --- */
SAME(sizeof(AVPacket), 80, "sizeof(AVPacket) != 80");
SAME(offsetof(AVPacket, pts), 8, "AVPacket.pts moved");
SAME(offsetof(AVPacket, dts), 16, "AVPacket.dts moved");
SAME(offsetof(AVPacket, data), 24, "AVPacket.data moved");
SAME(offsetof(AVPacket, size), 28, "AVPacket.size moved");
SAME(offsetof(AVPacket, stream_index), 32, "AVPacket.stream_index moved");
SAME(offsetof(AVPacket, flags), 36, "AVPacket.flags moved");
SAME(offsetof(AVPacket, duration), 48, "AVPacket.duration moved");
SAME(offsetof(AVPacket, time_base), 72, "AVPacket.time_base moved");

/* --- AVCodecParameters: ff.rs models a read-only PREFIX and must NEVER allocate one. sizeof is
       176 here and 136 on n3.3, and avcodec_parameters_from_context memsets the full size — so a
       stack copy sized by our model is a stack smash. venc allocates through the library. --- */
SAME(sizeof(AVCodecParameters), 168, "sizeof(AVCodecParameters) != 168 — do not stack-allocate it");
SAME(offsetof(AVCodecParameters, codec_type), 0, "AVCodecParameters.codec_type moved");
SAME(offsetof(AVCodecParameters, codec_id), 4, "AVCodecParameters.codec_id moved");
SAME(offsetof(AVCodecParameters, extradata), 12, "AVCodecParameters.extradata moved");
SAME(offsetof(AVCodecParameters, extradata_size), 16, "AVCodecParameters.extradata_size moved");
SAME(offsetof(AVCodecParameters, format), 28, "AVCodecParameters.format moved");
SAME(offsetof(AVCodecParameters, width), 56, "AVCodecParameters.width moved");
SAME(offsetof(AVCodecParameters, height), 60, "AVCodecParameters.height moved");
/* The stream-level side-data list, walked for the Dolby Vision configuration record, and the
   three colour fields logged beside it. All five were modelled in ff.rs long before anything
   read them, which is exactly why they are asserted now rather than then. */
SAME(offsetof(AVCodecParameters, coded_side_data), 20, "AVCodecParameters.coded_side_data moved");
SAME(offsetof(AVCodecParameters, nb_coded_side_data), 24, "AVCodecParameters.nb_coded_side_data moved");
SAME(offsetof(AVCodecParameters, color_primaries), 88, "AVCodecParameters.color_primaries moved");
SAME(offsetof(AVCodecParameters, color_trc), 92, "AVCodecParameters.color_trc moved");
SAME(offsetof(AVCodecParameters, color_space), 96, "AVCodecParameters.color_space moved");
SAME(offsetof(AVCodecParameters, sample_rate), 136, "AVCodecParameters.sample_rate moved");
/* FFmpeg 7 deleted the deprecated `channels` int; nb_channels inside ch_layout replaced it, and
   reading the old field on a hand-written model is a silent read of whatever now sits there. */
SAME(offsetof(AVCodecParameters, ch_layout), 112, "AVCodecParameters.ch_layout moved");
SAME(offsetof(AVChannelLayout, nb_channels), 4, "AVChannelLayout.nb_channels moved");

/* --- AVPacketSideData: one entry of the list above. `size` is a size_t, so the whole struct is
       12 bytes on 32-bit ARM and 24 on the 64-bit host — ff.rs spells that field `usize` so the
       model is right on both. --- */
SAME(sizeof(AVPacketSideData), 12, "sizeof(AVPacketSideData) != 12");
SAME(offsetof(AVPacketSideData, data), 0, "AVPacketSideData.data moved");
SAME(offsetof(AVPacketSideData, size), 4, "AVPacketSideData.size moved");
SAME(offsetof(AVPacketSideData, type), 8, "AVPacketSideData.type moved");

/* --- AVDOVIDecoderConfigurationRecord: nine plain uint8_t, no padding on any target. Asserted
       anyway because the header says its size "is not a part of the public ABI", and because a
       field inserted in the middle would silently renumber every DV profile the app logs and
       gates on. AV_PKT_DATA_DOVI_CONF is a sequential enum member, so it moves the same way. --- */
SAME(sizeof(AVDOVIDecoderConfigurationRecord), 9, "sizeof(AVDOVIDecoderConfigurationRecord) != 9");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, dv_profile), 2, "DOVI dv_profile moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, dv_level), 3, "DOVI dv_level moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, rpu_present_flag), 4, "DOVI rpu_present_flag moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, el_present_flag), 5, "DOVI el_present_flag moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, bl_present_flag), 6, "DOVI bl_present_flag moved");
SAME(offsetof(AVDOVIDecoderConfigurationRecord, dv_bl_signal_compatibility_id), 7, "DOVI bl_signal_compatibility_id moved");
SAME(AV_PKT_DATA_DOVI_CONF, 29, "AV_PKT_DATA_DOVI_CONF != 29 — the side-data enum shifted");

/* --- AVSubtitle: avcodec_decode_subtitle2 writes into ff.rs's stack copy, so this size IS
       load-bearing. Unchanged from n3.3. --- */
SAME(sizeof(AVSubtitle), 32, "sizeof(AVSubtitle) != 32 — ff.rs stack-allocates this one");
SAME(offsetof(AVSubtitle, num_rects), 12, "AVSubtitle.num_rects moved");
SAME(offsetof(AVSubtitle, rects), 16, "AVSubtitle.rects moved");
SAME(offsetof(AVSubtitle, pts), 24, "AVSubtitle.pts moved");

/* --- AVSubtitleRect: sizeof 132 -> 68, because FFmpeg 5.0 deleted the embedded deprecated
       AVPicture that used to push `data` all the way out to +84. --- */
SAME(sizeof(AVSubtitleRect), 68, "sizeof(AVSubtitleRect) != 68");
SAME(offsetof(AVSubtitleRect, w), 8, "AVSubtitleRect.w moved");
SAME(offsetof(AVSubtitleRect, h), 12, "AVSubtitleRect.h moved");
SAME(offsetof(AVSubtitleRect, data), 20, "AVSubtitleRect.data != 20 (the AVPicture is gone)");
SAME(offsetof(AVSubtitleRect, linesize), 36, "AVSubtitleRect.linesize moved");
SAME(offsetof(AVSubtitleRect, type), 56, "AVSubtitleRect.type moved");

/* --- AVBSFContext: `AVBSFInternal *internal` was removed in 5.0, pulling the rest up 4 bytes. --- */
SAME(offsetof(AVBSFContext, par_in), 12, "AVBSFContext.par_in moved");
SAME(offsetof(AVBSFContext, time_base_in), 20, "AVBSFContext.time_base_in moved");

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
