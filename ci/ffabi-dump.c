/* Emit the FFmpeg struct offsets ff.rs needs, for the TARGET architecture, without running
 * anything on it.
 *
 * `ci/ffabi-assert.c` CHECKS a table; this one DERIVES it. You need both, and only ever need this
 * when the bundled FFmpeg version changes: run it, paste the numbers into ff.rs, and the assert
 * file then holds them in place forever.
 *
 * The trick is that we cannot execute an ARM binary here. So each value becomes the SIZE of a
 * zero-initialised array, which lands in the object file's symbol table where `nm -S` can read it
 * — a compile-time constant recovered without a target, an emulator, or a device.
 *
 *   arm-webos-linux-gnueabi-gcc -I<headers> -c ci/ffabi-dump.c -o /tmp/d.o
 *   arm-webos-linux-gnueabi-nm -S --defined-only /tmp/d.o | sed -n 's/.* \([0-9a-f]*\) [Bb] plx_/\1 /p'
 *
 * (`tools/ffabi-dump.sh` runs exactly that and prints a table.) An offset of 0 would produce a
 * zero-size array, which some toolchains drop entirely — so every value is stored PLUS ONE, and
 * the runner subtracts it. That is why the numbers below all read `+ 1`.
 */
#include <stddef.h>
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavcodec/bsf.h>
#include <libavutil/frame.h>
#include <libavutil/dovi_meta.h>

/* Two ways to recover the same list, chosen by the platform's constraint rather than by taste.
 *
 * CROSS (default): each value becomes the SIZE of a zero-initialised array, read back with
 * `nm -S`. Nothing executes, which is the only option for an ARM object on this desk.
 *
 * `-DPLX_FFABI_MAIN` (the HOST table, for the simulator): the same list becomes a printed table,
 * because a host object CAN be run and because macOS `nm` reports every Mach-O size as zero — the
 * array trick is not merely unnecessary there, it silently yields nothing.
 *
 * **One list, two readers.** A second file would drift, and a drifted ABI table does not fail: it
 * succeeds with garbage, which is the whole reason this apparatus exists.
 */
#ifdef PLX_FFABI_MAIN
#include <stdio.h>
static const struct { const char *name; long value; } plx_table[] = {
#define DUMP(name, expr) { #name, (long)(expr) },
#else
#define DUMP(name, expr) char plx_##name[(expr) + 1];
#endif

DUMP(version_avformat, LIBAVFORMAT_VERSION_MAJOR)
DUMP(version_avcodec,  LIBAVCODEC_VERSION_MAJOR)
DUMP(version_avutil,   LIBAVUTIL_VERSION_MAJOR)

/* AVStream — read by offset; the struct is large and mostly internal. */
DUMP(off_stream_index,     offsetof(AVStream, index))
DUMP(off_stream_time_base, offsetof(AVStream, time_base))
DUMP(off_stream_codecpar,  offsetof(AVStream, codecpar))
DUMP(off_stream_metadata,  offsetof(AVStream, metadata))
DUMP(sizeof_stream,        sizeof(AVStream))

/* AVFormatContext — modelled through `nb_streams`/`streams`, plus duration by offset. */
DUMP(off_fmt_nb_streams, offsetof(AVFormatContext, nb_streams))
DUMP(off_fmt_streams,    offsetof(AVFormatContext, streams))
DUMP(off_fmt_pb,         offsetof(AVFormatContext, pb))
DUMP(off_fmt_duration,   offsetof(AVFormatContext, duration))

/* AVCodecContext — set by AVOption name, but the venc self-check wants these. */
DUMP(off_ctx_width,   offsetof(AVCodecContext, width))
DUMP(off_ctx_height,  offsetof(AVCodecContext, height))
DUMP(off_ctx_pix_fmt, offsetof(AVCodecContext, pix_fmt))

/* AVFrame — poked directly by the encode path. */
DUMP(off_frame_data,     offsetof(AVFrame, data))
DUMP(off_frame_linesize, offsetof(AVFrame, linesize))
DUMP(off_frame_width,    offsetof(AVFrame, width))
DUMP(off_frame_height,   offsetof(AVFrame, height))
DUMP(off_frame_format,   offsetof(AVFrame, format))
DUMP(off_frame_pts,      offsetof(AVFrame, pts))

/* Structs ff.rs models field-by-field with #[repr(C)]. A short model is a stack smash for the
 * two it allocates (AVCodecParameters, AVSubtitle), so these sizes are load-bearing. */
DUMP(sizeof_packet,      sizeof(AVPacket))
DUMP(sizeof_codecpar,    sizeof(AVCodecParameters))
DUMP(sizeof_subtitle,    sizeof(AVSubtitle))
DUMP(sizeof_subrect,     sizeof(AVSubtitleRect))
DUMP(sizeof_rational,    sizeof(AVRational))

DUMP(off_pkt_pts,          offsetof(AVPacket, pts))
DUMP(off_pkt_dts,          offsetof(AVPacket, dts))
DUMP(off_pkt_data,         offsetof(AVPacket, data))
DUMP(off_pkt_size,         offsetof(AVPacket, size))
DUMP(off_pkt_stream_index, offsetof(AVPacket, stream_index))
DUMP(off_pkt_flags,        offsetof(AVPacket, flags))
DUMP(off_pkt_duration,     offsetof(AVPacket, duration))

DUMP(off_par_codec_type, offsetof(AVCodecParameters, codec_type))
DUMP(off_par_codec_id,   offsetof(AVCodecParameters, codec_id))
DUMP(off_par_extradata,  offsetof(AVCodecParameters, extradata))
DUMP(off_par_extra_size, offsetof(AVCodecParameters, extradata_size))
DUMP(off_par_width,      offsetof(AVCodecParameters, width))
DUMP(off_par_height,     offsetof(AVCodecParameters, height))
DUMP(off_par_channels,   offsetof(AVCodecParameters, ch_layout))
DUMP(off_par_sample_rate, offsetof(AVCodecParameters, sample_rate))
DUMP(off_par_coded_side_data,    offsetof(AVCodecParameters, coded_side_data))
DUMP(off_par_nb_coded_side_data, offsetof(AVCodecParameters, nb_coded_side_data))
DUMP(off_par_color_primaries,    offsetof(AVCodecParameters, color_primaries))
DUMP(off_par_color_trc,          offsetof(AVCodecParameters, color_trc))
DUMP(off_par_color_space,        offsetof(AVCodecParameters, color_space))

/* AVPacketSideData — one entry of coded_side_data. `size` is a size_t, so sizeof is
 * architecture-dependent and this is one to re-derive rather than assume. */
DUMP(sizeof_pkt_side_data, sizeof(AVPacketSideData))
DUMP(off_psd_data,         offsetof(AVPacketSideData, data))
DUMP(off_psd_size,         offsetof(AVPacketSideData, size))
DUMP(off_psd_type,         offsetof(AVPacketSideData, type))

/* AVDOVIDecoderConfigurationRecord — nine uint8_t, but its size is explicitly not public ABI. */
DUMP(sizeof_dovi_conf,   sizeof(AVDOVIDecoderConfigurationRecord))
DUMP(off_dovi_profile,   offsetof(AVDOVIDecoderConfigurationRecord, dv_profile))
DUMP(off_dovi_level,     offsetof(AVDOVIDecoderConfigurationRecord, dv_level))
DUMP(off_dovi_el,        offsetof(AVDOVIDecoderConfigurationRecord, el_present_flag))
DUMP(off_dovi_bl_compat, offsetof(AVDOVIDecoderConfigurationRecord, dv_bl_signal_compatibility_id))

DUMP(off_sub_num_rects, offsetof(AVSubtitle, num_rects))
DUMP(off_sub_rects,     offsetof(AVSubtitle, rects))
DUMP(off_sub_pts,       offsetof(AVSubtitle, pts))
DUMP(off_rect_x,        offsetof(AVSubtitleRect, x))
DUMP(off_rect_y,        offsetof(AVSubtitleRect, y))
DUMP(off_rect_w,        offsetof(AVSubtitleRect, w))
DUMP(off_rect_h,        offsetof(AVSubtitleRect, h))
DUMP(off_rect_data,     offsetof(AVSubtitleRect, data))
DUMP(off_rect_linesize, offsetof(AVSubtitleRect, linesize))
DUMP(off_rect_type,     offsetof(AVSubtitleRect, type))

DUMP(off_bsf_par_in,       offsetof(AVBSFContext, par_in))
DUMP(off_bsf_time_base_in, offsetof(AVBSFContext, time_base_in))

/* Enum constants ff.rs hardcodes. */
DUMP(id_h264,   AV_CODEC_ID_H264)
DUMP(id_hevc,   AV_CODEC_ID_HEVC)
DUMP(id_aac,    AV_CODEC_ID_AAC)
DUMP(id_ac3,    AV_CODEC_ID_AC3)
DUMP(id_eac3,   AV_CODEC_ID_EAC3)
DUMP(sub_bitmap, SUBTITLE_BITMAP)
DUMP(pix_rgba,  AV_PIX_FMT_RGBA)
DUMP(pix_nv12,  AV_PIX_FMT_NV12)
DUMP(mt_video,  AVMEDIA_TYPE_VIDEO)
DUMP(mt_audio,  AVMEDIA_TYPE_AUDIO)
DUMP(mt_sub,    AVMEDIA_TYPE_SUBTITLE)

#ifdef PLX_FFABI_MAIN
};
int main(void) {
    for (unsigned i = 0; i < sizeof plx_table / sizeof plx_table[0]; i++)
        printf("%-24s %8ld   0x%lx\n", plx_table[i].name, plx_table[i].value,
               plx_table[i].value);
    return 0;
}
#endif
