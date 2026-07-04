/* mkv.c — streaming Matroska/EBML demuxer → H264 Annex-B AUs + raw audio
 * frames pushed to an au_queue; parses SeekHead/Cues for the seek index. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "aq.h"
#include "mkv.h"

/* ---- byte source ---- */
static int msrc_read(mkv_ctx *c, unsigned char *dst, int n) {
    int got = 0;
    while (got < n) {
        int r = c->read(c->ud, dst + got, n - got);
        if (r <= 0) { c->eof = 1; break; }
        got += r;
    }
    c->pos += got;
    return got;
}
static long long msrc_skip(mkv_ctx *c, long long n) {
    unsigned char tmp[8192];
    long long left = n;
    while (left > 0) {
        int chunk = left > (long long)sizeof tmp ? (int)sizeof tmp : (int)left;
        int r = msrc_read(c, tmp, chunk);
        if (r <= 0) break;
        left -= r;
    }
    return n - left;
}

/* ---- EBML primitives ---- */
/* element ID: keeps the length-marker bits (IDs are compared with them). */
int ebml_id(mkv_ctx *c, unsigned int *id, int *idlen) {
    unsigned char b0;
    if (msrc_read(c, &b0, 1) != 1) return 0;
    int len = 1; unsigned char mask = 0x80;
    while (!(b0 & mask)) { mask >>= 1; len++; if (len > 4) return 0; }
    unsigned int v = b0;
    for (int i = 1; i < len; i++) {
        unsigned char b;
        if (msrc_read(c, &b, 1) != 1) return 0;
        v = (v << 8) | b;
    }
    *id = v; if (idlen) *idlen = len;
    return 1;
}
/* element size (data size): strips the marker. *size = -1 for "unknown size". */
int ebml_size(mkv_ctx *c, long long *size, int *szlen) {
    unsigned char b0;
    if (msrc_read(c, &b0, 1) != 1) return 0;
    int len = 1; unsigned char mask = 0x80;
    while (!(b0 & mask)) { mask >>= 1; len++; if (len > 8) return 0; }
    long long v = b0 & (mask - 1);
    int all_ones = ((b0 & (mask - 1)) == (unsigned)(mask - 1));
    for (int i = 1; i < len; i++) {
        unsigned char b;
        if (msrc_read(c, &b, 1) != 1) return 0;
        v = (v << 8) | b;
        if (b != 0xFF) all_ones = 0;
    }
    if (all_ones) *size = -1; else *size = v;
    if (szlen) *szlen = len;
    return 1;
}
static long long ebml_uint(mkv_ctx *c, long long n) {
    unsigned char b[8]; long long v = 0;
    if (n < 1 || n > 8) { msrc_skip(c, n); return 0; }
    if (msrc_read(c, b, (int)n) != (int)n) return 0;
    for (int i = 0; i < n; i++) v = (v << 8) | b[i];
    return v;
}
static double ebml_float(mkv_ctx *c, long long n) {
    unsigned char b[8];
    if (n != 4 && n != 8) { msrc_skip(c, n); return 0; }
    if (msrc_read(c, b, (int)n) != (int)n) return 0;
    if (n == 4) { unsigned u = ((unsigned)b[0]<<24)|(b[1]<<16)|(b[2]<<8)|b[3];
                  float f; memcpy(&f, &u, 4); return (double)f; }
    unsigned long long u = 0; for (int i = 0; i < 8; i++) u = (u << 8) | b[i];
    double d; memcpy(&d, &u, 8); return d;
}

/* ---- avcC (AVCDecoderConfigurationRecord) → Annex-B SPS/PPS + NAL length size ---- */
static void mkv_parse_avcc(mkv_ctx *c, const unsigned char *p, int len) {
    if (len < 7 || p[0] != 1) return;
    c->nal_len_size = (p[4] & 0x03) + 1;
    int o = 5, out = 0;
    int nsps = p[o++] & 0x1f;
    for (int i = 0; i < nsps && o + 2 <= len; i++) {
        int l = (p[o] << 8) | p[o + 1]; o += 2;
        if (o + l > len || out + 4 + l > (int)sizeof c->sps_pps) break;
        c->sps_pps[out++]=0; c->sps_pps[out++]=0; c->sps_pps[out++]=0; c->sps_pps[out++]=1;
        memcpy(c->sps_pps + out, p + o, l); out += l; o += l;
    }
    if (o >= len) { c->sps_pps_len = out; return; }
    int npps = p[o++];
    for (int i = 0; i < npps && o + 2 <= len; i++) {
        int l = (p[o] << 8) | p[o + 1]; o += 2;
        if (o + l > len || out + 4 + l > (int)sizeof c->sps_pps) break;
        c->sps_pps[out++]=0; c->sps_pps[out++]=0; c->sps_pps[out++]=0; c->sps_pps[out++]=1;
        memcpy(c->sps_pps + out, p + o, l); out += l; o += l;
    }
    c->sps_pps_len = out;
}

/* ---- lacing: split a laced block body into frames (off/sz), returns count ----
 * fd/fl is the block data AFTER the flags byte. lacing: 0 none,1 Xiph,2 fixed,3 EBML */
static int mkv_unlace(const unsigned char *fd, int fl, int lacing,
                      int *off, int *sz, int maxf) {
    if (lacing == 0) { if (maxf < 1 || fl <= 0) return 0; off[0]=0; sz[0]=fl; return 1; }
    if (fl < 1) return 0;
    int nf = fd[0] + 1; int p = 1;
    if (nf > maxf) nf = maxf;
    if (lacing == 2) {                       /* fixed: equal sizes */
        if (nf <= 0) return 0;
        int each = (fl - p) / nf;
        for (int i = 0; i < nf; i++) { off[i] = p + i * each; sz[i] = each; }
        return nf;
    }
    if (lacing == 1) {                       /* Xiph */
        for (int i = 0; i < nf - 1; i++) {
            int s = 0; while (p < fl && fd[p] == 0xFF) { s += 255; p++; }
            if (p < fl) { s += fd[p]; p++; }
            sz[i] = s;
        }
        int o = p;
        for (int i = 0; i < nf - 1; i++) { off[i] = o; o += sz[i]; }
        off[nf-1] = o; sz[nf-1] = fl - o;
        return nf;
    }
    /* lacing == 3 EBML: first = unsigned vint, rest = prev + signed vint delta */
    {
        unsigned char b0 = fd[p]; int L = 1; unsigned char mk = 0x80;
        while (!(b0 & mk)) { mk >>= 1; L++; if (L > 8) return 0; }
        long long first = b0 & (mk - 1);
        for (int k = 1; k < L; k++) first = (first << 8) | fd[p + k];
        p += L;
        long long prev = first; sz[0] = (int)first;
        for (int i = 1; i < nf - 1; i++) {
            unsigned char c0 = fd[p]; int M = 1; unsigned char m2 = 0x80;
            while (!(c0 & m2)) { m2 >>= 1; M++; if (M > 8) return 0; }
            long long v = c0 & (m2 - 1);
            for (int k = 1; k < M; k++) v = (v << 8) | fd[p + k];
            p += M;
            prev += v - ((1LL << (7 * M - 1)) - 1);   /* signed delta */
            sz[i] = (int)prev;
        }
        int o = p;
        for (int i = 0; i < nf - 1; i++) { off[i] = o; o += sz[i]; }
        off[nf-1] = o; sz[nf-1] = fl - o;
        return nf;
    }
}

/* ---- one (Simple)Block → AU(s) → queue (video Annex-B, or raw audio frames) ---- */
static void mkv_handle_block(mkv_ctx *c, const unsigned char *blk, int len,
                             long long cluster_ts) {
    if (len < 4) return;
    /* track number: EBML vint (value, marker stripped) */
    int p = 0; unsigned char b0 = blk[0];
    int tl = 1; unsigned char mask = 0x80;
    while (!(b0 & mask)) { mask >>= 1; tl++; if (tl > 8) return; }
    long long track = b0 & (mask - 1);
    for (int i = 1; i < tl; i++) track = (track << 8) | blk[i];
    p = tl;
    if (p + 3 > len) return;
    int rel = (int)((short)((blk[p] << 8) | blk[p + 1])); p += 2;
    unsigned char flags = blk[p++];

    /* ---- audio track: unpack lacing, feed each raw frame (esData=2) ---- */
    if (c->has_audio && (int)track == c->atrack) {
        const unsigned char *afd = blk + p;
        int afl = len - p;
        int aoff[128], asz[128];
        int nf = mkv_unlace(afd, afl, (flags >> 1) & 0x03, aoff, asz, 128);
        long long base = (cluster_ts + rel) * c->tscale;
        for (int i = 0; i < nf; i++) {
            if (asz[i] <= 0 || aoff[i] + asz[i] > afl) continue;
            long long apts = base + (long long)i * c->audio_frame_ns;
            c->naus_a++;
            if (c->q) aq_push(c->q, afd + aoff[i], asz[i], apts, 1, 2);
        }
        return;
    }

    if ((int)track != c->vtrack || !c->is_h264) return;
    if ((flags >> 1) & 0x03) { c->laced_seen++; return; }   /* skip laced video (rare) */

    const unsigned char *fd = blk + p;
    int fl = len - p;
    int ns = c->nal_len_size;
    /* pass 1: is there an IDR (nal type 5)? → prepend SPS/PPS, mark keyframe */
    int key = 0;
    for (int i = 0; i + ns <= fl; ) {
        long L = 0; for (int k = 0; k < ns; k++) L = (L << 8) | fd[i + k];
        i += ns;
        if (L <= 0 || i + L > fl) break;
        if ((fd[i] & 0x1f) == 5) { key = 1; break; }
        i += L;
    }
    /* assemble AU */
    int need = fl + (fl / 32 + 4) + (key ? c->sps_pps_len : 0) + 64;
    if (need > c->scratch_cap) return;
    int out = 0;
    if (key && c->sps_pps_len) { memcpy(c->scratch, c->sps_pps, c->sps_pps_len); out = c->sps_pps_len; }
    for (int i = 0; i + ns <= fl; ) {
        long L = 0; for (int k = 0; k < ns; k++) L = (L << 8) | fd[i + k];
        i += ns;
        if (L <= 0 || i + L > fl) break;
        if (out + 4 + L > c->scratch_cap) break;
        c->scratch[out++]=0; c->scratch[out++]=0; c->scratch[out++]=0; c->scratch[out++]=1;
        memcpy(c->scratch + out, fd + i, L); out += (int)L;
        i += L;
    }
    if (out <= 0) return;
    long long pts = (cluster_ts + rel) * c->tscale;
    c->naus++; if (key) c->nkey++;
    if (c->q) aq_push(c->q, c->scratch, out, pts, key, 1);   /* es=1 video */
}

/* ---- element tree walk ---- */
static void mkv_parse_track_entry(mkv_ctx *c, long long size) {
    long long consumed = 0;
    int tnum = -1, ttype = -1;
    char codecid[40]; codecid[0] = 0;
    unsigned char cp[1024]; int cplen = 0;
    while (size < 0 || consumed < size) {
        unsigned int id; int il; long long sz; int sl;
        if (!ebml_id(c, &id, &il)) break;
        if (!ebml_size(c, &sz, &sl)) break;
        consumed += il + sl;
        if (id == 0xD7)      tnum  = (int)ebml_uint(c, sz);
        else if (id == 0x83) ttype = (int)ebml_uint(c, sz);
        else if (id == 0x86) { int n = sz < 39 ? (int)sz : 39; msrc_read(c,(unsigned char*)codecid,n); codecid[n]=0; if(sz>n)msrc_skip(c,sz-n); }
        else if (id == 0x63A2) { int n = sz < (long long)sizeof cp ? (int)sz : (int)sizeof cp; msrc_read(c,cp,n); cplen=n; if(sz>n)msrc_skip(c,sz-n); }
        else if (sz >= 0) msrc_skip(c, sz);
        else break;
        if (sz >= 0) consumed += sz;
    }
    if (ttype == 1 && c->vtrack < 0) {              /* first video track */
        c->vtrack = tnum;
        if (strncmp(codecid, "V_MPEG4/ISO/AVC", 15) == 0) {
            c->is_h264 = 1;
            if (cplen > 0) mkv_parse_avcc(c, cp, cplen);
        }
        if (c->debug) fprintf(stderr, "[mkv] video track=%d codec=%s h264=%d avcC=%dB nalLen=%d spspps=%dB\n",
                              tnum, codecid, c->is_h264, cplen, c->nal_len_size, c->sps_pps_len);
    }
    else if (ttype == 2 && c->atrack < 0) {         /* first supported audio track */
        /* AC3 = 1536 samples/frame, EAC3 = 1536, AAC = 1024 @ 48 kHz */
        if      (strncmp(codecid, "A_AC3",  5) == 0) { strcpy(c->acodec,"AC3");  c->audio_frame_ns = 32000000; c->has_audio = 1; }
        else if (strncmp(codecid, "A_EAC3", 6) == 0) { strcpy(c->acodec,"EAC3"); c->audio_frame_ns = 32000000; c->has_audio = 1; }
        else if (strncmp(codecid, "A_AAC",  5) == 0) { strcpy(c->acodec,"AAC");  c->audio_frame_ns = 21333333; c->has_audio = 1; }
        if (c->has_audio) c->atrack = tnum;
        if (c->debug) fprintf(stderr, "[mkv] audio track=%d codec=%s -> %s has_audio=%d\n",
                              tnum, codecid, c->acodec, c->has_audio);
    }
}

static void mkv_parse_cluster(mkv_ctx *c, long long size) {
    long long consumed = 0, cluster_ts = 0;
    while (size < 0 || consumed < size) {
        unsigned int id; int il; long long sz; int sl;
        if (!ebml_id(c, &id, &il)) break;
        if (!ebml_size(c, &sz, &sl)) break;
        consumed += il + sl;
        if (id == 0xE7) cluster_ts = ebml_uint(c, sz);
        else if (id == 0xA3) {                       /* SimpleBlock */
            if (sz >= 0 && sz <= c->scratch_cap) {
                unsigned char *blk = (unsigned char *)malloc((size_t)sz);
                if (blk) { msrc_read(c, blk, (int)sz); mkv_handle_block(c, blk, (int)sz, cluster_ts); free(blk); }
                else msrc_skip(c, sz);
            } else if (sz >= 0) msrc_skip(c, sz);
        }
        else if (id == 0xA0) {                        /* BlockGroup → find Block */
            long long bc = 0;
            while (sz < 0 || bc < sz) {
                unsigned int bid; int bil; long long bsz; int bsl;
                if (!ebml_id(c, &bid, &bil)) break;
                if (!ebml_size(c, &bsz, &bsl)) break;
                bc += bil + bsl;
                if (bid == 0xA1 && bsz >= 0 && bsz <= c->scratch_cap) {
                    unsigned char *blk = (unsigned char *)malloc((size_t)bsz);
                    if (blk) { msrc_read(c, blk, (int)bsz); mkv_handle_block(c, blk, (int)bsz, cluster_ts); free(blk); }
                    else msrc_skip(c, bsz);
                } else if (bsz >= 0) msrc_skip(c, bsz); else break;
                if (bsz >= 0) bc += bsz;
            }
        }
        else if (sz >= 0) msrc_skip(c, sz);
        else break;
        if (sz >= 0) consumed += sz;
        if (c->q && c->q->abort) break;
    }
}

static void mkv_parse_info(mkv_ctx *c, long long size) {
    long long consumed = 0;
    while (size < 0 || consumed < size) {
        unsigned int id; int il; long long sz; int sl;
        if (!ebml_id(c, &id, &il)) break;
        if (!ebml_size(c, &sz, &sl)) break;
        consumed += il + sl;
        if (id == 0x2AD7B1) c->tscale = ebml_uint(c, sz);
        else if (id == 0x4489) { double d = ebml_float(c, sz);
            c->duration_ns = (long long)(d * (double)(c->tscale > 0 ? c->tscale : 1000000)); }
        else if (sz >= 0) msrc_skip(c, sz); else break;
        if (sz >= 0) consumed += sz;
    }
}
static void mkv_parse_tracks(mkv_ctx *c, long long size) {
    long long consumed = 0;
    while (size < 0 || consumed < size) {
        unsigned int id; int il; long long sz; int sl;
        if (!ebml_id(c, &id, &il)) break;
        if (!ebml_size(c, &sz, &sl)) break;
        consumed += il + sl;
        if (id == 0xAE) mkv_parse_track_entry(c, sz);
        else if (sz >= 0) msrc_skip(c, sz); else break;
        if (sz >= 0) consumed += sz;
    }
}

/* SeekHead: locate the Cues element's byte position (rel. to segment data). */
static void mkv_parse_seekhead(mkv_ctx *c, long long size) {
    long long consumed = 0;
    while (size < 0 || consumed < size) {
        unsigned int id; int il; long long sz; int sl;
        if (!ebml_id(c, &id, &il)) break;
        if (!ebml_size(c, &sz, &sl)) break;
        consumed += il + sl;
        if (id == 0x4DBB) {                    /* Seek */
            unsigned int tgt = 0; long long pos = -1, sc = 0;
            while (sz < 0 || sc < sz) {
                unsigned int i2; int il2; long long s2; int sl2;
                if (!ebml_id(c, &i2, &il2)) break;
                if (!ebml_size(c, &s2, &sl2)) break;
                sc += il2 + sl2;
                if (i2 == 0x53AB) {            /* SeekID (bytes of the target element ID) */
                    unsigned char b[4] = {0}; int n = s2 < 4 ? (int)s2 : 4;
                    msrc_read(c, b, n); if (s2 > n) msrc_skip(c, s2 - n);
                    for (int k = 0; k < n; k++) tgt = (tgt << 8) | b[k];
                } else if (i2 == 0x53AC) pos = ebml_uint(c, s2);   /* SeekPosition */
                else if (s2 >= 0) msrc_skip(c, s2); else break;
                if (s2 >= 0) sc += s2;
            }
            if (tgt == 0x1C53BB6B && pos >= 0) c->cues_pos = pos;
        } else if (sz >= 0) msrc_skip(c, sz); else break;
        if (sz >= 0) consumed += sz;
    }
}
/* Cues: emit (CueTime, CueClusterPosition) per CuePoint via c->cue_cb. */
void mkv_parse_cues(mkv_ctx *c, long long size) {
    long long consumed = 0;
    while (size < 0 || consumed < size) {
        unsigned int id; int il; long long sz; int sl;
        if (!ebml_id(c, &id, &il)) break;
        if (!ebml_size(c, &sz, &sl)) break;
        consumed += il + sl;
        if (id == 0xBB) {                      /* CuePoint */
            long long ctime = -1, cbyte = -1, sc = 0;
            while (sz < 0 || sc < sz) {
                unsigned int i2; int il2; long long s2; int sl2;
                if (!ebml_id(c, &i2, &il2)) break;
                if (!ebml_size(c, &s2, &sl2)) break;
                sc += il2 + sl2;
                if (i2 == 0xB3) ctime = ebml_uint(c, s2);          /* CueTime */
                else if (i2 == 0xB7) {                             /* CueTrackPositions */
                    long long tc = 0;
                    while (s2 < 0 || tc < s2) {
                        unsigned int i3; int il3; long long s3; int sl3;
                        if (!ebml_id(c, &i3, &il3)) break;
                        if (!ebml_size(c, &s3, &sl3)) break;
                        tc += il3 + sl3;
                        if (i3 == 0xF1) cbyte = ebml_uint(c, s3);  /* CueClusterPosition */
                        else if (s3 >= 0) msrc_skip(c, s3); else break;
                        if (s3 >= 0) tc += s3;
                    }
                } else if (s2 >= 0) msrc_skip(c, s2); else break;
                if (s2 >= 0) sc += s2;
            }
            if (ctime >= 0 && cbyte >= 0 && c->cue_cb) c->cue_cb(c->cue_ud, ctime, cbyte);
        } else if (sz >= 0) msrc_skip(c, sz); else break;
        if (sz >= 0) consumed += sz;
    }
}

/* Segment children: Info / Tracks / Cluster (others skipped). */
static void mkv_parse_segment(mkv_ctx *c, long long size) {
    long long consumed = 0;
    while (size < 0 || consumed < size) {
        unsigned int id; int il; long long sz; int sl;
        if (!ebml_id(c, &id, &il)) break;
        if (!ebml_size(c, &sz, &sl)) break;
        consumed += il + sl;
        if      (id == 0x1549A966) mkv_parse_info(c, sz);
        else if (id == 0x1654AE6B) mkv_parse_tracks(c, sz);
        else if (id == 0x114D9B74) mkv_parse_seekhead(c, sz);   /* SeekHead → cues_pos */
        else if (id == 0x1F43B675) {                            /* Cluster */
            if (c->header_only) return;                         /* Cue preflight stops here */
            mkv_parse_cluster(c, sz);
        }
        else if (sz >= 0) msrc_skip(c, sz);
        else break;                       /* unknown-size non-container: bail */
        if (sz >= 0) consumed += sz;
        if (c->q && c->q->abort) break;
    }
}

/* Resume demux after a seek: scan for the next Cluster start (1F 43 B6 75),
 * then parse that cluster and all following ones to EOF. Reuses the track
 * config (vtrack/atrack/avcC/tscale) parsed on the first pass. Clusters begin
 * on a keyframe, so the decoder re-locks cleanly at the new position. */
void mkv_seek_run(mkv_ctx *c) {
    const unsigned char CID[4] = {0x1F, 0x43, 0xB6, 0x75};
    int matched = 0; unsigned char b;
    while (!c->eof) {
        if (msrc_read(c, &b, 1) != 1) return;
        if (b == CID[matched]) {
            if (++matched == 4) {
                long long sz; int sl;
                if (!ebml_size(c, &sz, &sl)) return;
                mkv_parse_cluster(c, sz);
                mkv_parse_segment(c, -1);   /* remaining clusters to EOF */
                return;
            }
        } else {
            matched = (b == CID[0]) ? 1 : 0;
        }
    }
}

/* Top level: EBML header, then Segment. Runs to EOF (or abort). */
int mkv_run(mkv_ctx *c) {
    if (c->tscale <= 0) c->tscale = 1000000;
    if (c->audio_frame_ns <= 0) c->audio_frame_ns = 32000000;
    c->vtrack = -1;
    c->atrack = -1;
    unsigned int id; int il; long long sz; int sl;
    while (ebml_id(c, &id, &il)) {
        if (!ebml_size(c, &sz, &sl)) break;
        if (id == 0x18538067) { c->segment_pos = c->pos;  /* Cue positions are rel. to here */
                                mkv_parse_segment(c, sz);
                                if (c->header_only) break; }  /* stop cleanly after the header */
        else if (sz >= 0) msrc_skip(c, sz);               /* EBML header etc. */
        else break;
        if (c->eof) break;
        if (c->q && c->q->abort) break;
    }
    return c->naus > 0 ? 0 : -1;   /* EOF signaled by the caller (seek loop) */
}
