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
 * TWO TABLES, ONE FILE. ff.rs carries an offset table per libavformat major (`Abi` — n3.3 for
 * webOS <=4.10.0, n4.x for 5.3.1..9.2.0), and this file asserts BOTH: the version macros select
 * which set of expectations applies, so compiling it against either vendored header tree proves
 * that tree's table. The Makefile compiles it twice. Keeping them together is deliberate — a
 * second copy of this file would drift from the first, and the interesting content is precisely
 * the DIFFERENCES, which are only legible side by side.
 *
 * TO SUPPORT A NEWER MAJOR (libavformat 59 = webOS 10.2.0, 60 = 11.2.0): drop that release's
 * headers beside the others, add an #elif arm here, and compile. Every constant that moved is
 * then reported by name, in one pass, on a machine — instead of being hunted with a disassembler
 * on hardware you may not own. That is the difference between a mechanical port and an
 * archaeological one. Note 59 is a bigger step than 58 was: FF_API_CONVERGENCE_DURATION and
 * FF_API_AVPICTURE both die there, so sizeof(AVPacket) and the whole AVSubtitleRect layout move
 * for the first time.
 */
#include <stddef.h>
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavutil/frame.h>

#define SAME(expr, want, what) _Static_assert((expr) == (want), what)

/* --- which table these headers describe. The three majors move together in every FFmpeg
   release, so a mismatched trio means someone mixed header trees. --- */
#if LIBAVFORMAT_VERSION_MAJOR == 57
  SAME(LIBAVCODEC_VERSION_MAJOR, 57, "libavformat 57 but libavcodec is not 57 — mixed headers");
  SAME(LIBAVUTIL_VERSION_MAJOR, 55, "libavformat 57 but libavutil is not 55 — mixed headers");
  /* ff.rs ABI_N33 */
  #define WANT_STREAM_TIME_BASE 40
  #define WANT_STREAM_CODECPAR  708
  #define WANT_FMT_DURATION     1064
  #define WANT_CTX_WIDTH        124
  #define WANT_CTX_HEIGHT       128
  #define WANT_CTX_PIX_FMT      144
  #define WANT_ID_H264          28
  #define WANT_ID_HEVC          174
  #define WANT_ID_EAC3          0x15029
  #define WANT_PIX_FMT_RGBA     28
#elif LIBAVFORMAT_VERSION_MAJOR == 58
  SAME(LIBAVCODEC_VERSION_MAJOR, 58, "libavformat 58 but libavcodec is not 58 — mixed headers");
  SAME(LIBAVUTIL_VERSION_MAJOR, 56, "libavformat 58 but libavutil is not 56 — mixed headers");
  /* ff.rs ABI_N4X. Eleven constants move from n3.3 and every one has a cause in the source:
     4.0 deleted a deprecated `AVFraction pts` from AVStream (time_base, codecpar), inserted
     `char *url` into AVFormatContext (duration), and dropped FF_API_XVMC and FF_API_VOXWARE,
     which renumbered every codec id below the entries those guards contributed. */
  #define WANT_STREAM_TIME_BASE 16
  #define WANT_STREAM_CODECPAR  176
  #define WANT_FMT_DURATION     1072
  #define WANT_CTX_WIDTH        92
  #define WANT_CTX_HEIGHT       96
  #define WANT_CTX_PIX_FMT      112
  #define WANT_ID_H264          27
  #define WANT_ID_HEVC          173
  #define WANT_ID_EAC3          0x15028
  #define WANT_PIX_FMT_RGBA     26
#else
  #error "no ff.rs Abi table for this libavformat major — add one here and in ff.rs, together"
#endif

/* --- named offsets: ff.rs OFF_STREAM_* / OFF_CTX_* / OFF_FRAME_* --- */
SAME(offsetof(AVStream, index), 0, "OFF_STREAM_INDEX != 0");
SAME(offsetof(AVStream, time_base), WANT_STREAM_TIME_BASE, "Abi::stream_time_base is wrong");
SAME(offsetof(AVStream, codecpar), WANT_STREAM_CODECPAR, "Abi::stream_codecpar is wrong");
/* AVFormatContext.duration — read by offset, not as a struct field, because 4.0 inserted
   `char *url` ahead of it. This is the assertion that makes that safe. */
SAME(offsetof(AVFormatContext, duration), WANT_FMT_DURATION, "Abi::fmt_duration is wrong");

/* NOT read by ff.rs today — venc sets width/height/pix_fmt through libavcodec's own AVOption
   table by name, which is immune to the offsets moving. Asserted anyway, for two reasons: the
   numbers are proven per major if anyone ever needs to poke them again, and the 124->92 shift is
   a compact demonstration that "the header layout changed" is a real thing on this port. */
SAME(offsetof(AVCodecContext, width), WANT_CTX_WIDTH, "AVCodecContext.width moved");
SAME(offsetof(AVCodecContext, height), WANT_CTX_HEIGHT, "AVCodecContext.height moved");
SAME(offsetof(AVCodecContext, pix_fmt), WANT_CTX_PIX_FMT, "AVCodecContext.pix_fmt moved");

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
SAME(AV_CODEC_ID_H264, WANT_ID_H264, "Abi::codec_id_h264 is wrong");
SAME(AV_CODEC_ID_HEVC, WANT_ID_HEVC, "Abi::codec_id_hevc is wrong");
/* E-AC3 shifts because AV_CODEC_ID_VOXWARE, the entry directly below it, was removed with
   FF_API_VOXWARE. Reading the old value on a webOS 5 set names ATRAC3P instead. */
SAME(AV_CODEC_ID_EAC3, WANT_ID_EAC3, "Abi::codec_id_eac3 is wrong");
SAME(AV_CODEC_ID_AAC, 0x15002, "AV_CODEC_ID_AAC moved (it sits above VOXWARE; it should not)");
SAME(AV_CODEC_ID_AC3, 0x15003, "AV_CODEC_ID_AC3 moved (it sits above VOXWARE; it should not)");
SAME(SUBTITLE_BITMAP, 1, "SUBTITLE_BITMAP != 1 (NONE is 0)");
/* Not a constant in ff.rs — it resolves this at runtime with av_get_pix_fmt("rgba"),
   which is why the value moving costs nothing. Asserted anyway: if it ever STOPPED
   moving between majors the runtime lookup would be removable. */
SAME(AV_PIX_FMT_RGBA, WANT_PIX_FMT_RGBA, "AV_PIX_FMT_RGBA is not what this major says");
