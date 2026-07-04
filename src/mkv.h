/* mkv.h — minimal streaming Matroska (MKV) demuxer that extracts the first H264
 * video track and emits Annex-B access units. Forward-only (no seeking): parses
 * EBML elements in file order, so Tracks (→ avcC/SPS/PPS) arrive before the first
 * Cluster. Reads bytes through a generic byte-source callback so the identical
 * parser runs on the host (against a file) and on-device (against http_stream).
 * Implementation in mkv.c. Scope: H264 (V_MPEG4/ISO/AVC), unlaced
 * SimpleBlock/Block, known element sizes (real Plex remuxes qualify). */
#ifndef PLEXPOC_MKV_H
#define PLEXPOC_MKV_H

#include "aq.h"

/* returns bytes read into dst (0 at EOF, <0 on error) */
typedef int (*mkv_byte_reader)(void *ud, unsigned char *dst, int n);

typedef struct {
    mkv_byte_reader read;
    void *ud;
    long long pos;              /* absolute byte offset consumed */
    int  eof;

    long long tscale;          /* ns per timestamp tick (default 1,000,000) */
    long long duration_ns;     /* total duration from Info (0 if absent) */
    long long segment_pos;     /* byte offset of Segment data start (Cue base) */
    long long cues_pos;        /* Cues byte offset rel. to segment_pos (from SeekHead) */
    int  header_only;          /* stop parsing at the first Cluster (for the Cue preflight) */
    void (*cue_cb)(void *ud, long long time_ticks, long long byte); /* per CuePoint */
    void *cue_ud;
    int  vtrack;               /* video track number, -1 until found */
    int  is_h264;
    int  nal_len_size;         /* 1..4, from avcC */
    unsigned char sps_pps[1024];
    int  sps_pps_len;          /* Annex-B SPS+PPS to prepend at each IDR */

    int  atrack;               /* audio track number, -1 until found */
    int  has_audio;            /* a supported audio track was selected */
    char acodec[8];            /* "AC3" / "EAC3" / "AAC" */
    long long audio_frame_ns;  /* per-frame duration for laced audio PTS */

    au_queue *q;               /* output (NULL = parse-only, for tests) */
    unsigned char *scratch;    /* AU assembly buffer */
    int  scratch_cap;

    /* stats / debug */
    long naus, nkey, naus_a;
    int  debug;
    int  laced_seen;
} mkv_ctx;

/* EBML element ID (keeps length-marker bits) — used by the Cue preflight too. */
int ebml_id(mkv_ctx *c, unsigned int *id, int *idlen);
/* EBML element size (strips marker; *size=-1 for unknown). */
int ebml_size(mkv_ctx *c, long long *size, int *szlen);
/* Top level: EBML header, then Segment. Runs to EOF (or abort). */
int mkv_run(mkv_ctx *c);
/* Resume demux after a seek: scan for the next Cluster, then parse to EOF. */
void mkv_seek_run(mkv_ctx *c);
/* Cues: emit (CueTime, CueClusterPosition) per CuePoint via c->cue_cb. */
void mkv_parse_cues(mkv_ctx *c, long long size);

#endif /* PLEXPOC_MKV_H */
