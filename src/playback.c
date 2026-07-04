/* playback.c — video-playback subsystem: LG StarfishMediaAPIs C++-from-C glue,
 * ACB video-plane binding, buffer-feed orchestration, demux/cue/load threads,
 * play_movie (direct-play vs transcode), and the transport HUD. Encapsulates the
 * Starfish ABI: main only touches it via the public API in playback.h. */
#include "app.h"
#include "gfx.h"
#include "text.h"
#include "pms.h"
#include "stream.h"
#include "aq.h"
#include "mkv.h"
#include "playback.h"
#include "starfish.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <pthread.h>

/* The StarfishMediaAPIs (C++) + ACB video-plane ABI lives in the starfish.c seam
 * (the flat sf_ / acb_ verbs). This file owns only the buffer-feed orchestration
 * and calls those verbs; library-thread callbacks land in sf_on_event/acb_on_event. */
static long g_acb = 0;           /* the ACB id (from acb_create) — availability flag */
static int  videoInfoSent = 0;   /* ACB setMediaVideoData sent once, after PLAYING */
/* /tmp/poc-ptype: ACB playerType for acb_create (default MSE=10). */
static int  g_ptype = PLAYER_TYPE_MSE;

/* ACB library-thread event (forwarded by the seam) */
void acb_on_event(long ev, const char *reply) {
    if (elogf) {
        fprintf(elogf, "acb_cb ev=%ld reply=%s\n", ev, reply ? reply : "");
        fflush(elogf);
    }
}

/* thin transport wrappers so main never touches Starfish */
void playback_pause(void)  { sf_pause(); }
void playback_resume(void) { sf_play(); }

/* Create + initialize the ACB (App Common Binding) that binds the decoded video
 * sink to the display plane. We deliberately DON'T register our own
 * com.webos.media client — that collides with the uMS connection
 * StarfishMediaAPIs registers (which then fails acquire with CONN_FIND_ERR).
 * The pipeline owns that connection; we only need ACB for the plane bind. */
int acb_init(void) {
    {
        FILE *pf = fopen("/tmp/poc-ptype", "r");   /* dev: ACB playerType override */
        if (pf) { fscanf(pf, "%d", &g_ptype); fclose(pf); }
        if (elogf) { fprintf(elogf, "ptype=%d\n", g_ptype); fflush(elogf); }
    }
    const char *appId = getenv("APPID");
    g_acb = acb_create(appId, g_ptype);
    if (elogf) { fprintf(elogf, "acb create=%ld\n", g_acb); fflush(elogf); }
    return 1;
}

/* ================= buffer-feed playback (StarfishMediaAPIs) ================= */
/* Validation build: read a raw H264 Annex-B sample from /tmp/sample.h264,
 * split into access units (each starts at an AUD: 00 00 00 01 09), and feed
 * them to an in-process pipeline while ACB binds the video plane. */
static unsigned char *bf_data = NULL;   /* whole sample in memory */
static long bf_len = 0;
static long bf_au[40000];               /* AU start offsets */
static int  bf_naus = 0, bf_next = 0, bf_loop = 0;
static int  bf_loaded = 0, bf_bound = 0, bf_playing = 0;
int         bf_started = 0;   /* shared with main (playback.h) */
static char bf_mediaId[64] = "";
static const char *bf_payload = NULL;
static void *load_thread(void *arg);   /* fwd */

/* THE payload tv.display/setMediaVideoData actually parses: the WHOLE sourceInfo
 * envelope {"context":..,"content":..,"video":{frameRate,scanType,width,height,
 * pixelAspectRatio:{w,h},data3D:{...},bitRate,adaptive,path,afd,rotation,hfr,..}}.
 * Proven by the handler's own debug log: it reads context, content, PAR_W/PAR_H,
 * the 3D fields, bAdaptive, Path -- all sourceInfo fields nested under "video".
 * The flat display schema or the bare video object make every field parse 0/null. */
static char sourceInfoRaw[4096] = "";
volatile int bf_frames = 0;             /* decoded-frame events (type 0) seen (shared) */

/* ---- player UI/transport state (shared with main via playback.h) ---- */
volatile long long g_playpos_ns = 0;  /* displayed position (wall-clock driven) */
long long pl_dur_ns = 0;              /* total duration (from MKV Info) */
int       pl_paused = 0;
int       resumePausePending = 0;     /* re-pause once a resume seek's frame is shown */
unsigned  pl_hud_until = 0;           /* SDL ticks: HUD auto-hides after */
long long pl_scrub_ns = -1;           /* scrub preview target (-1 = not scrubbing) */
/* Seek keeps the FED pts continuous so the pipeline never sees a jump: fed_pts =
 * real_pts + g_pts_shift. After a seek, g_rebase_pending recomputes the shift
 * from the first new AU so playback continues from the last fed pts. */
static volatile long long g_pts_shift = 0;
static long long g_max_fed_pts = 0;
static volatile int g_rebase_pending = 0;

/* pipeline library-thread event, forwarded by the seam (was starfish_cb).
 * (eventType, numValue, jsonStr). Feeding goes through sf_feed() in the seam. */
void sf_on_event(int type, long long num, const char *str) {
    if (elogf && type != 0) {   /* skip the per-frame "presented" event (~24/s) */
        fprintf(elogf, "smp_cb type=%d num=%lld str=%.1400s\n",
                type, num, str ? str : "");
        fflush(elogf);
    }
    if (type == 0) {   /* a frame was PRESENTED (num = its fed pts). This event goes
                        * silent during post-seek loading, so the position naturally
                        * freezes then instead of the wall clock counting stale time. */
        bf_frames++;
        g_playpos_ns = num - g_pts_shift;   /* map fed pts → real content position */
    }
    if (!str) return;
    /* grab the pipeline mediaId once available — the source-info event (type 4)
     * carries it as "context":"_...."; also accept "mediaId" */
    if (!bf_mediaId[0]) {
        const char *m = strstr(str, "\"context\":\"");
        int off = 11;
        if (!m) { m = strstr(str, "\"mediaId\":\""); }
        if (m) { m += off; const char *q = strchr(m, '"');
            if (q && (size_t)(q - m) < sizeof bf_mediaId) {
                memcpy(bf_mediaId, m, q - m); bf_mediaId[q - m] = 0;
                if (elogf) { fprintf(elogf, "SMP context/mediaId=%s\n", bf_mediaId); fflush(elogf); } } }
    }
    if (!bf_loaded && (strstr(str, "loadCompleted") || strstr(str, "\"loaded\""))) {
        bf_loaded = 1;
        if (elogf) { fprintf(elogf, "SMP loadCompleted id=%s\n", bf_mediaId); fflush(elogf); }
    }
    /* Capture the WHOLE sourceInfo envelope verbatim — this is the exact payload
     * tv.display/setMediaVideoData parses (context + content + nested video). */
    if (!sourceInfoRaw[0] && strstr(str, "\"video\":") && strstr(str, "\"context\":")) {
        size_t n = strlen(str);
        if (n + 1 < sizeof sourceInfoRaw) {
            memcpy(sourceInfoRaw, str, n + 1);
            if (elogf) { fprintf(elogf, "SMP sourceInfoRaw captured (%zu bytes)\n", n); fflush(elogf); }
        }
    }
}

/* split Annex-B into AUs on the 5-byte AUD prefix 00 00 00 01 09 */
static void bf_split(void) {
    bf_naus = 0;
    for (long i = 0; i + 4 < bf_len && bf_naus < (int)(sizeof bf_au / sizeof bf_au[0]); i++) {
        if (bf_data[i]==0 && bf_data[i+1]==0 && bf_data[i+2]==0 &&
            bf_data[i+3]==1 && bf_data[i+4]==0x09) {
            bf_au[bf_naus++] = i;
            i += 4;
        }
    }
    if (elogf) { fprintf(elogf, "bf_split: %d AUs in %ld bytes\n", bf_naus, bf_len); fflush(elogf); }
}

/* ---- streaming path: PMS over HTTP → MKV demux → AU queue ---- */
static au_queue     g_aq;
static int          bf_stream = 0;      /* 1 = stream from PMS, 0 = /tmp/sample.h264 */
extern char         g_url[1024];               /* stream URL — owned by the Rust route module */
extern char         g_transcode_session[64];   /* transcode session to stop on teardown (Rust route) */
static au_node     *bf_pending = NULL;  /* AU popped but not yet accepted (BufferFull) */
static http_stream  g_hs;
static mkv_ctx      g_mkv;
static pthread_t    g_stream_th, g_load_th;
static int          g_stream_created = 0, g_load_created = 0;
static long long          g_file_size  = 0;    /* full part size (from first GET) */
static volatile long long g_seek_byte  = -1;   /* demux thread: reposition here */
volatile long long g_seek_to_ns = -1;   /* UI request: seek to this time (shared) */
static char         g_host[256] = ""; static int g_port = 32400;
static const char  *g_path = "/";
/* MKV Cue index (accurate time→byte) fetched by a preflight thread.
 * Dynamically grown — a movie can have any number of keyframes. */
static struct cue_ent { long long t_ns; long long byte; } *g_cues = NULL;
static int          g_ncues = 0, g_cues_cap = 0;
static volatile int g_cues_ready = 0;
static long long    g_segment_pos = 0;
static http_stream  g_hs2;
static pthread_t    g_cues_th;               /* the preflight thread (joinable) */
static int          g_cues_created = 0;
static volatile int g_cues_abort = 0;        /* signal the preflight to stop before we free g_cues */
static int hs2_reader(void *ud, unsigned char *dst, int n) { return http_read((http_stream *)ud, dst, n); }
static void cue_cb(void *ud, long long time_ticks, long long byte) {
    mkv_ctx *c = (mkv_ctx *)ud;
    if (g_cues_abort) return;                    /* teardown in progress — don't touch g_cues */
    if (g_ncues == g_cues_cap) {                 /* grow (amortized doubling) */
        int ncap = g_cues_cap ? g_cues_cap * 2 : 4096;
        void *n = realloc(g_cues, (size_t)ncap * sizeof *g_cues);
        if (!n) return;
        g_cues = (struct cue_ent *)n; g_cues_cap = ncap;
    }
    g_cues[g_ncues].t_ns = time_ticks * c->tscale;
    g_cues[g_ncues].byte = g_segment_pos + byte;   /* absolute file offset of the cluster */
    g_ncues++;
}
/* preflight: parse the header to find the Cues, then fetch + parse them */
static mkv_ctx g_cmkv;
static void *cues_thread(void *arg) {
    (void)arg;
    if (elogf) { fprintf(elogf, "cues: preflight start %s:%d\n", g_host, g_port); fflush(elogf); }
    memset(&g_cmkv, 0, sizeof g_cmkv);
    g_cmkv.header_only = 1;
    if (http_open(&g_hs2, g_host, g_port, g_path, NULL) != 0) {
        if (elogf) { fprintf(elogf, "cues: preflight http_open FAILED\n"); fflush(elogf); } return NULL; }
    g_cmkv.read = hs2_reader; g_cmkv.ud = &g_hs2;
    mkv_run(&g_cmkv);                 /* stops at first Cluster; sets segment_pos, cues_pos, tscale */
    http_close(&g_hs2);
    g_segment_pos = g_cmkv.segment_pos;
    if (elogf) { fprintf(elogf, "cues: header parsed segpos=%lld cuespos=%lld tscale=%lld\n",
                         g_cmkv.segment_pos, g_cmkv.cues_pos, g_cmkv.tscale); fflush(elogf); }
    if (g_cmkv.cues_pos <= 0 || g_cmkv.segment_pos <= 0) {
        if (elogf) { fprintf(elogf, "cues: none (segpos=%lld cuespos=%lld)\n", g_cmkv.segment_pos, g_cmkv.cues_pos); fflush(elogf); }
        return NULL;
    }
    if (g_cues_abort) return NULL;               /* teardown began during the header parse */
    long long cues_abs = g_cmkv.segment_pos + g_cmkv.cues_pos;
    char rh[80]; snprintf(rh, sizeof rh, "Range: bytes=%lld-\r\n", cues_abs);
    if (http_open(&g_hs2, g_host, g_port, g_path, rh) != 0) return NULL;
    if (g_cues_abort) { http_close(&g_hs2); return NULL; }   /* abort raced the reopen */
    g_cmkv.read = hs2_reader; g_cmkv.ud = &g_hs2; g_cmkv.eof = 0; g_cmkv.pos = 0;
    g_cmkv.cue_cb = cue_cb; g_cmkv.cue_ud = &g_cmkv;
    unsigned int id; int il; long long sz; int sl;
    if (ebml_id(&g_cmkv, &id, &il) && id == 0x1C53BB6B && ebml_size(&g_cmkv, &sz, &sl))
        mkv_parse_cues(&g_cmkv, sz);
    http_close(&g_hs2);
    if (!g_cues_abort) g_cues_ready = 1;         /* don't mark a partial/interrupted fetch ready */
    if (elogf) { fprintf(elogf, "cues: %d points (tscale=%lld segpos=%lld abort=%d)\n", g_ncues, g_cmkv.tscale, g_segment_pos, g_cues_abort); fflush(elogf); }
    return NULL;
}
/* nearest cue at or before t (returns absolute byte, or -1 if none) */
static long long cue_byte_for(long long t) {
    if (!g_cues_ready || g_ncues == 0) return -1;
    /* g_cues is appended in increasing t_ns → binary-search the last cue <= t */
    int lo = 0, hi = g_ncues - 1, best = -1;
    while (lo <= hi) {
        int mid = (lo + hi) / 2;
        if (g_cues[mid].t_ns <= t) { best = mid; lo = mid + 1; }
        else hi = mid - 1;
    }
    return g_cues[best < 0 ? 0 : best].byte;   /* none <= t → seek to the first cue */
}
/* parse http://HOST[:PORT]/PATH?query from g_url into g_host/g_port/g_path */
static void parse_stream_url(void) {
    const char *p = g_url;
    if (strncmp(p, "http://", 7) == 0) p += 7;
    size_t hi = 0;
    while (*p && *p != ':' && *p != '/' && hi < sizeof g_host - 1) g_host[hi++] = *p++;
    g_host[hi] = 0;
    if (*p == ':') { g_port = atoi(p + 1); while (*p && *p != '/') p++; }
    g_path = (*p == '/') ? p : "/";
}

static int hs_reader(void *ud, unsigned char *dst, int n) {
    return http_read((http_stream *)ud, dst, n);
}

/* demux thread: opens the PMS part URL and runs the MKV demuxer, pushing AUs to
 * g_aq. Loops to support seeking: when the pump sets g_seek_byte (and closes the
 * socket to interrupt the read), re-opens with a byte Range and resyncs to the
 * next cluster, reusing the track config parsed on the first pass. */
static void *stream_thread(void *arg) {
    (void)arg;
    if (elogf) { fprintf(elogf, "stream: host=%s port=%d path=%.80s\n", g_host, g_port, g_path); fflush(elogf); }
    memset(&g_mkv, 0, sizeof g_mkv);
    g_mkv.q = &g_aq;
    g_mkv.scratch_cap = 4 * 1024 * 1024;
    g_mkv.scratch = (unsigned char *)malloc(g_mkv.scratch_cap);

    long long start = 0; int first = 1;
    for (;;) {
        char rh[80]; const char *extra = NULL;
        if (start > 0) { snprintf(rh, sizeof rh, "Range: bytes=%lld-\r\n", start); extra = rh; }
        if (http_open(&g_hs, g_host, g_port, g_path, extra) != 0) {
            if (elogf) { fprintf(elogf, "stream: http_open FAILED status=%d\n", g_hs.status); fflush(elogf); }
            break;
        }
        if (first) g_file_size = g_hs.content_length;   /* full size only from the un-ranged GET */
        if (elogf) { fprintf(elogf, "stream: open status=%d start=%lld clen=%lld filesize=%lld\n",
                             g_hs.status, start, g_hs.content_length, g_file_size); fflush(elogf); }
        g_mkv.read = hs_reader; g_mkv.ud = &g_hs; g_mkv.eof = 0; g_mkv.pos = 0;
        if (first) { mkv_run(&g_mkv); first = 0; }
        else       { mkv_seek_run(&g_mkv); }
        http_close(&g_hs);
        if (g_aq.abort) break;
        long long sb = g_seek_byte;
        if (sb >= 0) { g_seek_byte = -1; start = sb;
            if (elogf) { fprintf(elogf, "stream: seek → byte %lld\n", start); fflush(elogf); }
            continue; }
        break;   /* real EOF, no pending seek */
    }
    aq_set_eof(&g_aq);
    if (elogf) { fprintf(elogf, "stream: demux ended AUs=%ld audio=%ld\n", g_mkv.naus, g_mkv.naus_a); fflush(elogf); }
    return NULL;
}

int start_bufferfeed(void) {
    /* A caller may pre-set g_url (a movie selected from the gallery); only then
     * fall back to the /tmp/poc-url dev override. Either way, a non-empty g_url
     * means stream mode and wins over the DEMO fallback below. */
    if (!g_url[0]) {
        FILE *uf = fopen("/tmp/poc-url", "r");
        if (uf) {
            if (fgets(g_url, sizeof g_url, uf)) {
                size_t l = strlen(g_url);
                while (l && (g_url[l-1]=='\n' || g_url[l-1]=='\r' || g_url[l-1]==' ')) g_url[--l] = 0;
            }
            fclose(uf);
        }
    }
    if (g_url[0]) bf_stream = 1;
    if (!bf_stream) {
        FILE *f = fopen("/tmp/sample.h264", "rb");
        if (!f) {
            /* no override + no local sample → stream the built-in demo movie */
            strncpy(g_url, DEMO_STREAM_URL, sizeof g_url - 1);
            bf_stream = 1;
        } else {
            fseek(f, 0, SEEK_END); bf_len = ftell(f); fseek(f, 0, SEEK_SET);
            bf_data = malloc(bf_len);
            if (!bf_data || fread(bf_data, 1, bf_len, f) != (size_t)bf_len) { fclose(f); return 0; }
            fclose(f);
            bf_split();
            if (bf_naus < 2) return 0;
        }
    }

    /* BUFFERSTREAM, raw ES. Load() parses with boost ptree and REQUIRES the
     * top-level {"args":[ ... ]} wrapper (ss4s shape). srcBufferLevelVideo max
     * raised to 8 MB for real-content keyframes. */
    static const char *payload_v =              /* file sample: video-only */
        "{\"args\":[{\"mediaTransportType\":\"BUFFERSTREAM\",\"option\":{"
        "\"appId\":\"com.glin.plexpoc\","
        "\"externalStreamingInfo\":{\"contents\":{"
        "\"codec\":{\"video\":\"H264\"},"
        "\"esInfo\":{\"pauseAtDecodeTime\":false,\"ptsToDecode\":0,"
        "\"seperatedPTS\":true},"
        "\"format\":\"RAW\",\"provider\":\"plexpoc\"},"
        "\"streamQualityInfo\":true,\"audioSync\":true,\"restartStreaming\":false,"
        "\"bufferingCtrInfo\":{\"bufferMaxLevel\":0,\"bufferMinLevel\":0,"
        "\"preBufferByte\":0,\"qBufferLevelAudio\":0,\"qBufferLevelVideo\":0,"
        "\"srcBufferLevelAudio\":{\"minimum\":1,\"maximum\":32768},"
        "\"srcBufferLevelVideo\":{\"minimum\":1,\"maximum\":8388608}}},"
        "\"needAudio\":false,\"queryPosition\":false,\"lowDelayMode\":true,"
        "\"transmission\":{\"contentsType\":\"LIVE\"},"
        "\"adaptiveStreaming\":{\"audioOnly\":false,\"maxWidth\":1920,"
        "\"maxHeight\":1080,\"maxFrameRate\":30}}}]}";
    /* stream mode: H264 video + AC3 audio (Frozen). TODO: parameterize audio
     * codec once we probe the container header before Load. */
    static const char *payload_av =
        "{\"args\":[{\"mediaTransportType\":\"BUFFERSTREAM\",\"option\":{"
        "\"appId\":\"com.glin.plexpoc\","
        "\"externalStreamingInfo\":{\"contents\":{"
        "\"codec\":{\"video\":\"H264\",\"audio\":\"AC3\"},"
        "\"esInfo\":{\"pauseAtDecodeTime\":false,\"ptsToDecode\":0,"
        "\"seperatedPTS\":true},"
        "\"format\":\"RAW\",\"provider\":\"plexpoc\"},"
        "\"streamQualityInfo\":true,\"audioSync\":true,\"restartStreaming\":false,"
        "\"bufferingCtrInfo\":{\"bufferMaxLevel\":0,\"bufferMinLevel\":0,"
        "\"preBufferByte\":0,\"qBufferLevelAudio\":0,\"qBufferLevelVideo\":0,"
        "\"srcBufferLevelAudio\":{\"minimum\":1,\"maximum\":1048576},"
        "\"srcBufferLevelVideo\":{\"minimum\":1,\"maximum\":8388608}}},"
        "\"needAudio\":true,\"queryPosition\":false,\"lowDelayMode\":false,"
        "\"transmission\":{\"contentsType\":\"LIVE\"},"
        "\"adaptiveStreaming\":{\"audioOnly\":false,\"maxWidth\":1920,"
        "\"maxHeight\":1080,\"maxFrameRate\":30}}}]}";
    bf_payload = bf_stream ? payload_av : payload_v;

    if (bf_stream) {
        parse_stream_url();
        aq_init(&g_aq);
        pthread_create(&g_stream_th, NULL, stream_thread, NULL);
        g_stream_created = 1;
        /* Skip the Cue preflight for a transcode: a live transcode has no byte-Cues
         * (seek uses &offset=), and a 2nd connection to the same session makes the
         * server cut the main demux stream before any Cluster arrives. */
        if (!g_cues_ready && !g_transcode_session[0]) {
            g_cues_abort = 0;               /* joinable so stop_bufferfeed can wait for it */
            if (pthread_create(&g_cues_th, NULL, cues_thread, NULL) == 0) g_cues_created = 1;
        }
    }
    /* the media thread constructs + loads + runs the loop (owns the context) */
    pthread_create(&g_load_th, NULL, load_thread, NULL);
    g_load_created = 1;
    if (elogf) { fprintf(elogf, "SMP: media thread spawned, stream=%d naus=%d\n", bf_stream, bf_naus); fflush(elogf); }
    bf_started = 1;
    return 1;
}

/* Stop playback: unblock+join threads, unload+destruct the pipeline, release the
 * video plane, and reset all state so a fresh start_bufferfeed() can restart. */
void stop_bufferfeed(int keep_cues) {
    if (!bf_started) return;
    /* Stop the detached-no-more cue preflight FIRST and JOIN it before anything
     * frees g_cues — cue_cb writes g_cues on the preflight thread, so freeing it
     * from under a still-running thread is a use-after-free. */
    g_cues_abort = 1;
    if (bf_stream) { aq_abort(&g_aq); http_close(&g_hs); http_close(&g_hs2); }  /* unblock threads */
    if (g_cues_created)   { pthread_join(g_cues_th, NULL);   g_cues_created = 0; }
    if (g_stream_created) { pthread_join(g_stream_th, NULL); g_stream_created = 0; }
    if (g_load_created)   { pthread_join(g_load_th, NULL);   g_load_created = 0; }
    if (sf_ready()) {
        sf_unload();
        if (g_acb) acb_unload();
        sf_destroy();
    }
    if (bf_stream) { int eof; au_node *n; while ((n = aq_pop(&g_aq, &eof))) free(n);
                     aq_destroy(&g_aq); }   /* paired with aq_init in start_bufferfeed */
    if (bf_pending) { free(bf_pending); bf_pending = NULL; }
    free(bf_data); bf_data = NULL;
    bf_started = bf_loaded = bf_bound = bf_playing = 0;
    bf_stream = 0;
    bf_next = bf_naus = bf_loop = bf_frames = 0;
    videoInfoSent = 0;
    bf_mediaId[0] = 0; sourceInfoRaw[0] = 0;
    /* free the server-side transcode encoder if this playback was a transcode */
    if (g_transcode_session[0]) {
        char sp[256]; http_stream shs;
        snprintf(sp, sizeof sp, "/video/:/transcode/universal/stop?session=%s"
                 "&X-Plex-Client-Identifier=%s&X-Plex-Token=%s",
                 g_transcode_session, g_transcode_session, PMS_TOKEN);
        if (http_open(&shs, PMS_HOST, PMS_PORT, sp, NULL) == 0) http_close(&shs);
        g_transcode_session[0] = 0;
    }
    pl_paused = 0; resumePausePending = 0; g_playpos_ns = 0; pl_dur_ns = 0; g_url[0] = 0;
    g_file_size = 0; g_seek_byte = -1; g_seek_to_ns = -1;
    g_pts_shift = 0; g_max_fed_pts = 0; g_rebase_pending = 0; pl_scrub_ns = -1;
    /* keep the cue index across an app-switch (same file) so the resume seek is
     * accurate immediately instead of falling back to the CBR estimate. Only keep
     * a FULLY-loaded table — a partial one would make the resume re-fetch append
     * duplicates (cue_cb grows g_cues from g_ncues). */
    if (!keep_cues || !g_cues_ready) { free(g_cues); g_cues = NULL; g_ncues = 0;
                                       g_cues_cap = 0; g_cues_ready = 0; g_segment_pos = 0; }
    if (elogf) { fprintf(elogf, "stop_bufferfeed: torn down\n"); fflush(elogf); }
}

/* feed AUs ahead as fast as the pipeline accepts; PTS schedules presentation.
 * loops the short sample (continuous PTS) so playback doesn't EOS after ~6s */
static void bf_feed_ahead(void) {
    int fed = 0;
    while (fed < 60) {
        if (bf_next >= bf_naus - 1) { bf_next = 0; bf_loop++; } /* loop sample */
        long off = bf_au[bf_next];
        long end = bf_au[bf_next + 1];
        long long pts = ((long long)bf_loop * (bf_naus - 1) + bf_next)
                        * 41708333LL; /* continuous ns @ 23.976 */
        char r = sf_feed(bf_data + off, (unsigned)(end - off), pts, 1);
        if (elogf && bf_loop == 0 && bf_next < 3) {
            fprintf(elogf, "feed AU#%d sz=%ld reply=%c\n", bf_next,
                    end - off, r ? r : '?'); fflush(elogf);
        }
        if (r != 'O') break;             /* 'B'=BufferFull → next tick */
        bf_next++;
        fed++;
    }
}

/* feed streamed AUs from the demux queue; hold the current AU across ticks on
 * BufferFull (backpressure) instead of dropping it. */
static void bf_feed_stream(void) {
    int fed = 0;
    while (fed < 120) {
        if (!bf_pending) {
            int eof = 0;
            bf_pending = aq_pop(&g_aq, &eof);
            if (!bf_pending) break;
        }
        /* after a seek, drop everything until the first video keyframe, then
         * zero-base the fed timeline on it (a flushed decoder needs a keyframe
         * first anyway; this also discards any stale pre-seek AUs) */
        if (g_rebase_pending) {
            if (bf_pending->es == 1 && bf_pending->key) {
                g_pts_shift = -bf_pending->pts; g_rebase_pending = 0;
            } else { free(bf_pending); bf_pending = NULL; continue; }
        }
        long long fed_pts = bf_pending->pts + g_pts_shift;
        /* drop stale AUs (a big backward jump beyond B-frame reorder distance) */
        if (fed_pts < g_max_fed_pts - 2000000000LL) {
            free(bf_pending); bf_pending = NULL; continue;
        }
        if (fed_pts < 0) fed_pts = 0;
        char r = sf_feed(bf_pending->data, (unsigned)bf_pending->len, fed_pts, bf_pending->es);
        if (fed_pts > g_max_fed_pts) g_max_fed_pts = fed_pts;
        static long vtot = 0, atot = 0;
        if (bf_pending->es == 1) vtot++; else atot++;
        if (elogf && (bf_pending->es == 1) && (vtot <= 4 || vtot % 100 == 0)) {
            fprintf(elogf, "feed v#%ld sz=%d fed=%lld reply=%c qbytes=%ld\n",
                    vtot, bf_pending->len, fed_pts, r ? r : '?', aq_bytes(&g_aq)); fflush(elogf); }
        if (r != 'O') break;             /* keep bf_pending, retry next tick */
        free(bf_pending); bf_pending = NULL;
        fed++;
    }
}

/* Construct + Load inline (like Kodi/ss4s). The library owns its OWN
 * GMainContext + loop + message thread, so Load returns immediately and
 * callbacks arrive on the library's thread — no app-run loop needed.
 * CRITICAL: construct with uid=NULL. That registers the pipeline on the
 * pre-authorized com.webos.media.client._<uid> namespace via LSRegister;
 * passing our appId instead forces LSRegisterApplicationService (privileged),
 * which a sideloaded app has no role for → uMS acquire fails CONN_FIND_ERR. */
static void *load_thread(void *arg) {
    (void)arg;
    if (elogf) { fprintf(elogf, "SMP: calling Load (uid=NULL)\n"); fflush(elogf); }
    int ok = sf_load(bf_payload);   /* seam: ctor(uid=NULL) + notifyForeground + Load */
    if (elogf) { fprintf(elogf, "SMP: Load returned ok=%d\n", ok); fflush(elogf); }
    return NULL;
}

/* called from the main loop: once loaded, bind ACB, Play, then feed AUs */
void bufferfeed_pump(unsigned now) {
    if (!bf_started || !sf_ready()) return;   /* wait for media-thread ctor */
    (void)now;

    /* pending seek: flush the pipeline, drop queued AUs, and tell the demux
     * thread to re-open the HTTP stream at the byte offset for the target time. */
    if (bf_stream && g_seek_to_ns >= 0 && bf_playing && g_file_size > 0 && pl_dur_ns > 0) {
        long long t = g_seek_to_ns; g_seek_to_ns = -1;
        if (t < 0) t = 0;
        sf_flush();                   /* drop decoded/queued frames; resets the clock to ~0 */
        sf_set_playtime(0);
        sf_play();                    /* resume presentation after the flush */
        { int eof; au_node *n; while ((n = aq_pop(&g_aq, &eof))) free(n); }
        if (bf_pending) { free(bf_pending); bf_pending = NULL; }
        long long byte = cue_byte_for(t);          /* accurate: MKV Cue index */
        if (byte < 0)                              /* cues not ready → CBR estimate */
            byte = (long long)((double)t / (double)pl_dur_ns * (double)g_file_size);
        if (byte < 0) byte = 0;
        g_seek_byte = byte;
        http_close(&g_hs);            /* unblock the demux read → it re-opens at byte */
        /* zero-base the fed timeline on the first post-seek video keyframe (set in
         * bf_feed_stream), so it lands at pts 0 and presents against the flush-reset
         * clock immediately — no catch-up freeze */
        g_rebase_pending = 1;
        g_max_fed_pts = 0;
        bf_frames = 0;                /* count only POST-seek frames (resume re-pause gate) */
        g_playpos_ns = t;             /* displayed position jumps; wall clock takes over */
        if (elogf) { fprintf(elogf, "seek: t=%lld byte=%lld\n", t, byte); fflush(elogf); }
    }

    if (!bf_loaded && sf_is_load_completed()) {
        bf_loaded = 1;
        if (elogf) { fprintf(elogf, "SMP loadCompleted\n"); fflush(elogf); }
    }
    /* NB: getMediaID() returns empty for buffer-feed — the real pipeline mediaId
     * arrives as "context" in the type-4 sourceInfo callback (see starfish_cb).
     * Polling getMediaID here is useless AND crashes under demux-thread contention
     * (SIGSEGV inside libplayerAPIs), so it's removed. */
    /* Play as soon as loaded so the pipeline decodes fed frames */
    if (bf_loaded && !bf_playing) {
        sf_play();
        bf_playing = 1;
        if (elogf) { fprintf(elogf, "SMP Play\n"); fflush(elogf); }
    }
    /* ACB bind, Kodi/ss4s order (they NEVER call stopMute — ACB auto-unmutes
     * once the transaction validates with accepted video data). NO display
     * window yet: that comes WITH the video data, once the decoder is running:
     *   loadCompleted → setSinkType(MAIN) → setMediaId → setState(LOADED)
     *   [decoder produces frames] → setMediaVideoData(VERBATIM) →
     *   setDisplayWindow → setState(PLAYING) */
    if (bf_loaded && !bf_bound && g_acb && bf_mediaId[0]) {
        bf_bound = 1;
        acb_bind(bf_mediaId);
        if (elogf) { fprintf(elogf, "SMP ACB bound id=%s\n", bf_mediaId); fflush(elogf); }
    }
    /* Send the WHOLE sourceInfo envelope (context + content + video) VERBATIM,
     * once the decoder is producing frames so the videosink is bound to MAIN.
     * The envelope's "context" is our pipeline id (= the VSM MAIN-sink context),
     * so tv.display resolves the video and every video.* field parses correctly.
     * Then window + PLAYING (VSM connect already auto-unmuted the plane). */
    if (bf_bound && !videoInfoSent && sourceInfoRaw[0] && bf_frames >= 2) {
        int rv = acb_send_video_data(sourceInfoRaw);
        if (elogf) { fprintf(elogf, "setMediaVideoData rv=%d frames=%d payload=%.240s\n",
                             rv, bf_frames, sourceInfoRaw); fflush(elogf); }
        if (rv != -1) {   /* -1 = client-side isJsonError reject; else accepted */
            videoInfoSent = 1;
            acb_start(0, 0, 1920, 1080);
            if (elogf) { fprintf(elogf, "setMediaVideoData sent → window+PLAYING\n"); fflush(elogf); }
        }
    }
    /* feed AUs once playing (Feed only succeeds after Play). Don't feed while a
     * seek is still armed (g_seek_to_ns>=0): on a resume the seek is armed BEFORE
     * the pipeline reaches PLAYING, so feeding here first would present the file
     * start for a frame before the seek repositions — a visible jump. Holding off
     * lets the seek block (above) drain those start AUs and reposition first. */
    if (bf_stream && g_mkv.duration_ns > 0) pl_dur_ns = g_mkv.duration_ns;
    if (bf_playing && !pl_paused && g_seek_to_ns < 0) {
        if (bf_stream) bf_feed_stream(); else bf_feed_ahead();
    }
}

/* ---- playback HUD (Apple TV-style) ---- */
extern char g_title[128];    /* HUD title — owned by the Rust route module */
extern char g_ctxline[96];   /* HUD context line — owned by the Rust route module */

static void fmt_time(char *out, int cap, long long ns, int neg) {
    long t = ns > 0 ? (long)(ns / 1000000000LL) : 0;
    int h = (int)(t / 3600), m = (int)((t / 60) % 60), s = (int)(t % 60);
    if (h > 0) snprintf(out, cap, "%s%d:%02d:%02d", neg ? "-" : "", h, m, s);
    else       snprintf(out, cap, "%s%d:%02d", neg ? "-" : "", m, s);
}
/* crude but recognizable control glyphs built from rects */
static void icon_subs(float x, float y, float s, const float c[4]) {
    float d[4] = {0, 0, 0, 0.62f};
    draw_rect(x, y + s * 0.06f, s, s * 0.66f, 0, 7, c, c, 0);
    draw_rect(x + s * 0.16f, y + s * 0.24f, s * 0.68f, s * 0.10f, 0, 2, d, d, 0);
    draw_rect(x + s * 0.16f, y + s * 0.44f, s * 0.42f, s * 0.10f, 0, 2, d, d, 0);
}
static void icon_audio(float x, float y, float s, const float c[4]) {
    draw_rect(x,              y + s * 0.34f, s * 0.15f, s * 0.42f, 0, 2, c, c, 0);
    draw_rect(x + s * 0.28f,  y + s * 0.12f, s * 0.15f, s * 0.64f, 0, 2, c, c, 0);
    draw_rect(x + s * 0.56f,  y + s * 0.26f, s * 0.15f, s * 0.50f, 0, 2, c, c, 0);
    draw_rect(x + s * 0.84f,  y + s * 0.44f, s * 0.15f, s * 0.32f, 0, 2, c, c, 0);
}
static void icon_pip(float x, float y, float s, const float c[4]) {
    float d[4] = {0, 0, 0, 0.62f};
    draw_rect(x, y + s * 0.10f, s, s * 0.62f, 0, 6, c, c, 0);       /* screen */
    draw_rect(x + s * 0.10f, y + s * 0.20f, s * 0.80f, s * 0.42f, 0, 4, d, d, 0);
    draw_rect(x + s * 0.46f, y + s * 0.36f, s * 0.44f, s * 0.30f, 0, 3, c, c, 0); /* inset */
}
/* rounded-square icon button; which: 0 subs, 1 audio, 2 pip */
static void draw_iconbtn(float x, float y, float s, int which, int focused) {
    float bg[4], gc[4];
    if (focused) { bg[0]=0.96f;bg[1]=0.96f;bg[2]=0.98f;bg[3]=0.97f; gc[0]=0.06f;gc[1]=0.07f;gc[2]=0.09f;gc[3]=1; }
    else         { bg[0]=1;bg[1]=1;bg[2]=1;bg[3]=0.15f;             gc[0]=0.94f;gc[1]=0.95f;gc[2]=1;gc[3]=1; }
    draw_rect(x, y, s, s, 0, 15, bg, bg, 0);
    float pad = s * 0.28f, gs = s - 2 * pad, gx = x + pad, gy = y + pad;
    if      (which == 0) icon_subs(gx, gy, gs, gc);
    else if (which == 1) icon_audio(gx, gy, gs, gc);
    else                 icon_pip(gx, gy, gs, gc);
}
void draw_hud(void) {
    /* bottom scrim: transparent → dark, so text reads over bright video */
    float clr[4] = {0, 0, 0, 0.0f}, drk[4] = {0, 0, 0, 0.86f};
    draw_rect(0, SCR_H - 470, SCR_W, 470, 0, 0, clr, drk, 0);

    float white[4] = {0.98f, 0.98f, 1.0f, 1.0f};
    float dim[4]   = {0.72f, 0.74f, 0.80f, 0.95f};
    float track[4] = {1.0f, 1.0f, 1.0f, 0.24f};

    float mx = 90;
    /* context line + bold title */
    draw_text(g_ctxline, mx, SCR_H - 312, 24, dim, 0, 0);
    draw_text(g_title,   mx, SCR_H - 278, 54, white, 0, 1);

    /* right control buttons: subtitles, audio, picture-in-picture */
    float bs = 58, bby = SCR_H - 288, bbx = SCR_W - 90 - bs;
    draw_iconbtn(bbx, bby, bs, 2, 0);  bbx -= bs + 22;
    draw_iconbtn(bbx, bby, bs, 1, 0);  bbx -= bs + 22;
    draw_iconbtn(bbx, bby, bs, 0, 0);

    /* scrubber: thick translucent track, solid-white progress. During playback
     * the playhead is a thin vertical line (round knob is only for scrubbing). */
    float sx = mx, sw = SCR_W - 2 * mx, sy = SCR_H - 198, sh = 8;
    long long dispos = (pl_scrub_ns >= 0) ? pl_scrub_ns : g_playpos_ns;  /* preview while scrubbing */
    double frac = (pl_dur_ns > 0) ? (double)dispos / (double)pl_dur_ns : 0.0;
    if (frac < 0) frac = 0; if (frac > 1) frac = 1;
    draw_rect(sx, sy, sw, sh, 0, sh * 0.5f, track, track, 0);         /* track: rounded capsule */
    float fw = (float)(sw * frac);
    if (fw > sh * 0.5f)                                               /* fill: round-left, flat-right */
        draw_rrect(sx, sy, fw, sh, sh * 0.5f, 0.0f, white);
    else if (fw > 0)
        draw_rrect(sx, sy, fw, sh, fw * 0.5f, 0.0f, white);
    float hx = sx + fw;
    if (pl_scrub_ns >= 0)    /* scrubbing: round knob */
        draw_rect(hx - 9, sy + sh * 0.5f - 9, 18, 18, 0, 9, white, white, 0);
    else                     /* playing: thin flat playhead line */
        draw_rect(hx - 1.5f, sy - 4, 3, sh + 8, 0, 0, white, white, 0);

    /* elapsed under the playhead; remaining at right. A small state glyph shows
     * ONLY when paused (nothing during normal playback). */
    char te[32], tr[32];
    fmt_time(te, sizeof te, dispos, 0);
    fmt_time(tr, sizeof tr, pl_dur_ns - dispos, 1);
    float ty = sy + 26;
    float ew = draw_text(te, hx - 12, ty, 24, white, 0, 0);
    if (pl_paused) {
        float gx = hx - 12 + ew + 14, gs = 20, gyy = ty + 3, w = gs * 0.32f;
        draw_rect(gx, gyy, w, gs, 0, 2, white, white, 0);
        draw_rect(gx + w * 1.9f, gyy, w, gs, 0, 2, white, white, 0);
    }
    draw_text(tr, sx + sw, ty, 24, dim, 2, 0);

    /* plain-text tabs below-left (no pill backgrounds) */
    float tabdim[4] = {0.68f, 0.70f, 0.76f, 0.95f};
    float px = mx, py = SCR_H - 122;
    px += draw_text("Info", px, py, 28, white, 0, 1) + 44;
    draw_text("Chapters", px, py, 28, tabdim, 0, 1);
}

/* play_movie (direct-play vs transcode route selection + HUD strings) moved to
 * the Rust route module (rust-modules/src/route.rs); it writes g_url /
 * g_transcode_session / g_title / g_ctxline, which the functions above read. */

