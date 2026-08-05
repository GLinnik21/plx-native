/* Compile-time proof that rust-modules/src/ff.rs's FFmpeg ABI table is the real n3.3 layout.
 *
 * WHY THIS FILE EXISTS. The TV ships FFmpeg n3.3 stripped and with no headers, so ff.rs reads
 * FFmpeg structs at offsets that were originally derived BY HAND — by disassembling the device's
 * own libraries, and re-derived that way several times. The numbers are right, but nothing in the
 * build could say so: a slip produced a wild pointer on a television, not an error on a desk.
 *
 * FFmpeg's public headers settle it for free. Layout there is a function of the version macros
 * ALONE — there is not a single `#if CONFIG_*` in the public headers — so a vendor build at the
 * same version cannot have a different layout, whatever LG passed to configure. Compiling
 * `offsetof` against the matching release therefore re-derives every constant exactly, and
 * disagreement becomes a compile error naming the field.
 *
 * This translation unit is COMPILED, NEVER LINKED, and contains no code — only assertions.
 * It is a Makefile prerequisite of the Rust staticlib, so `make` fails before producing a binary.
 *
 * TO SUPPORT A NEW FFmpeg MAJOR (i.e. webOS 5+): drop that release's headers beside the n3.3 set,
 * copy this file, and compile it. Every constant that moved is then reported by name, in one
 * pass, on a machine — instead of being hunted with a disassembler on hardware you may not own.
 * That is the difference between a mechanical port and an archaeological one.
 */
#include <stddef.h>
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavutil/frame.h>

#define SAME(expr, want, what) _Static_assert((expr) == (want), what)

/* --- the version this table describes. If these fire, the headers are the wrong release. --- */
SAME(LIBAVFORMAT_VERSION_MAJOR, 57, "headers are not libavformat 57");
SAME(LIBAVCODEC_VERSION_MAJOR, 57, "headers are not libavcodec 57");
SAME(LIBAVUTIL_VERSION_MAJOR, 55, "headers are not libavutil 55");

/* --- named offsets: ff.rs OFF_STREAM_* / OFF_CTX_* / OFF_FRAME_* --- */
SAME(offsetof(AVStream, index), 0, "OFF_STREAM_INDEX != 0");
SAME(offsetof(AVStream, time_base), 40, "OFF_STREAM_TIME_BASE != 40");
SAME(offsetof(AVStream, codecpar), 708, "OFF_STREAM_CODECPAR != 708");

SAME(offsetof(AVCodecContext, width), 124, "OFF_CTX_WIDTH != 124");
SAME(offsetof(AVCodecContext, height), 128, "OFF_CTX_HEIGHT != 128");
SAME(offsetof(AVCodecContext, pix_fmt), 144, "OFF_CTX_PIX_FMT != 144");

SAME(offsetof(AVFrame, data), 0, "OFF_FRAME_DATA != 0");
SAME(offsetof(AVFrame, linesize), 32, "OFF_FRAME_LINESIZE != 32");
SAME(offsetof(AVFrame, width), 68, "OFF_FRAME_WIDTH != 68");
SAME(offsetof(AVFrame, height), 72, "OFF_FRAME_HEIGHT != 72");
SAME(offsetof(AVFrame, format), 80, "OFF_FRAME_FORMAT != 80");
/* +104, not +100: a 4-byte pad 8-aligns the int64 on ARM EABI. The classic AVFrame-on-ARM trap,
   and the one most likely to be got wrong by reading a struct definition on a 64-bit machine. */
SAME(offsetof(AVFrame, pts), 104, "OFF_FRAME_PTS != 104 (ARM EABI int64 alignment)");

/* --- structs ff.rs models field-by-field with #[repr(C)]: the sizes it depends on --- */
SAME(sizeof(AVPacket), 72, "sizeof(AVPacket) != 72");
SAME(sizeof(AVCodecParameters), 136, "sizeof(AVCodecParameters) != 136");
SAME(sizeof(AVSubtitle), 32, "sizeof(AVSubtitle) != 32");
SAME(sizeof(AVSubtitleRect), 132, "sizeof(AVSubtitleRect) != 132");

/* AVCodecParameters is written by the library into a STACK allocation in ff.rs's venc
   self-check, so a short model there is a stack smash rather than a bad read. */
SAME(offsetof(AVCodecParameters, codec_id), 4, "AVCodecParameters.codec_id moved");
SAME(offsetof(AVCodecParameters, width), 48, "AVCodecParameters.width moved");
SAME(offsetof(AVCodecParameters, height), 52, "AVCodecParameters.height moved");

/* Likewise AVSubtitle: `avcodec_decode_subtitle2` writes into ff.rs's stack copy. */
SAME(offsetof(AVSubtitle, num_rects), 12, "AVSubtitle.num_rects moved");
SAME(offsetof(AVSubtitle, rects), 16, "AVSubtitle.rects moved");
SAME(offsetof(AVSubtitle, pts), 24, "AVSubtitle.pts moved");
SAME(offsetof(AVSubtitleRect, w), 8, "AVSubtitleRect.w moved");
SAME(offsetof(AVSubtitleRect, h), 12, "AVSubtitleRect.h moved");
/* +84 despite `data` being the 5th member: an entire deprecated AVPicture is embedded ahead of
   it under FF_API_AVPICTURE, which is exactly the kind of thing hand-derivation gets wrong. */
SAME(offsetof(AVSubtitleRect, data), 84, "AVSubtitleRect.data != 84 (embedded AVPicture)");
SAME(offsetof(AVSubtitleRect, linesize), 100, "AVSubtitleRect.linesize moved");

/* --- enum constants ff.rs hardcodes --- */
SAME(AV_CODEC_ID_H264, 28, "AV_CODEC_ID_H264 != 28 (27 from avutil 56 — wrong major)");
SAME(AV_CODEC_ID_HEVC, 174, "AV_CODEC_ID_HEVC != 174 (173 from avutil 56 — wrong major)");
SAME(SUBTITLE_BITMAP, 1, "SUBTITLE_BITMAP != 1 (NONE is 0)");
SAME(AV_PIX_FMT_RGBA, 28, "AV_PIX_FMT_RGBA != 28");
