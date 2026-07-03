/* plexpoc — native webOS GLES2 UI proof-of-concept
 * Apple TV-style shelf UI: rounded-corner cards (SDF shader), spring
 * animations, D-pad focus, FPS counter. Links against the TV's own
 * SDL2 (LG webOS port) and GLESv2.
 */
#define _GNU_SOURCE          /* strcasestr */
#define SDL_MAIN_HANDLED
#include <SDL2/SDL.h>
#include <SDL2/SDL_syswm.h>
#include <GLES2/gl2.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <unistd.h>
#include <signal.h>
#include <ucontext.h>
#include <pthread.h>
#include "stream.h"
#include "aq.h"
#include "mkv.h"

/* SDL2_ttf (real impl on the TV); SDL_Color/SDL_Surface come from SDL.h */
typedef struct _TTF_Font TTF_Font;
extern int  TTF_Init(void);
extern TTF_Font *TTF_OpenFont(const char *file, int ptsize);
extern SDL_Surface *TTF_RenderUTF8_Blended(TTF_Font *font, const char *text, SDL_Color fg);
extern int  TTF_SizeUTF8(TTF_Font *font, const char *text, int *w, int *h);
extern void TTF_SetFontStyle(TTF_Font *font, int style);   /* 0x01 = BOLD */

/* Wayland: make our GL surface non-opaque so the starfish video plane
 * below shows through. webOS LSM marks app windows opaque by default and
 * ignores buffer alpha; we must explicitly clear the opaque region.
 * The TV's SDL is 2.0.4 (no transparency hint), so we drive the wayland
 * proxy directly. wl_surface opcodes: 4=set_opaque_region, 6=commit. */
extern void wl_proxy_marshal(void *proxy, unsigned opcode, ...);
extern int wl_display_flush(void *display);
static void *g_wl_surface = NULL, *g_wl_display = NULL;

static void clear_opaque_region(void) {
    if (!g_wl_surface) return;
    /* set_opaque_region(NULL) only. Surface state is double-buffered and
     * applied on the next commit — let SDL_GL_SwapWindow do that commit
     * (a bare commit here, before SDL attaches a buffer, presents a
     * null-buffer surface and disrupts the slaved video plane). */
    wl_proxy_marshal(g_wl_surface, 4, (void *)0);
}

/* LG webOS extension in the TV's SDL fork: soft-hide the Magic Remote
 * cursor exactly like system apps do (system re-shows it on motion). */
extern int SDL_webOSCursorVisibility(int visible);

/* ---- starfish playback via com.webos.media over luna-service2.
 * The jail only allows this app to register on the bus as ITSELF, so we
 * link libluna-service2 (present in the jail) and keep the connection
 * alive for the app's lifetime — the pipeline lives as long as the
 * client connection does. Minimal extern decls; no LS2/glib headers. ---- */
/* Demo / test PMS part URLs. The real ones carry a private X-Plex-Token, so they
 * live in src/config.local.h (gitignored) which overrides these placeholders.
 * Copy src/config.local.h.example → src/config.local.h and fill in your PMS host
 * + token. At runtime, writing a part URL to /tmp/poc-url overrides either. */
#if defined(__has_include)
#  if __has_include("config.local.h")
#    include "config.local.h"
#  endif
#endif
/* Demo movie played when OK is pressed on a shelf card (Frozen, H264+AC3 MKV). */
#ifndef DEMO_STREAM_URL
#  define DEMO_STREAM_URL "http://YOUR_PMS_HOST:32400/library/parts/0/0/file.mkv?X-Plex-Token=YOUR_PLEX_TOKEN"
#endif

typedef int (*LSFilterCb)(void *sh, void *msg, void *ctx);
/* LSError layout (luna-service2, 32-bit ARM): int + 4 ptrs + magic */
struct LSErr { int code; char *message; const char *file; int line;
               const char *func; void *pad; unsigned long magic; };
extern int  LSErrorInit(struct LSErr *e);
extern void LSErrorFree(struct LSErr *e);
extern int LSRegister(const char *name, void **sh, struct LSErr *lserror);
extern int LSCall(void *sh, const char *uri, const char *payload,
                  LSFilterCb cb, void *ctx, unsigned long *token,
                  void *lserror);
extern int LSCallOneReply(void *sh, const char *uri, const char *payload,
                          LSFilterCb cb, void *ctx, unsigned long *token,
                          void *lserror);
extern int LSGmainAttach(void *sh, void *mainloop, void *lserror);
extern const char *LSMessageGetPayload(void *msg);
extern int g_main_context_iteration(void *ctx, int may_block);
extern int g_main_context_pending(void *ctx);

/* ---- ACB (App Common Binding): binds the decoded video sink to a display
 * window. Without it the starfish pipeline never presents (audio included).
 * Sequence derived from Kodi's webOS Starfish renderer. ---- */
extern long AcbAPI_create(void);
extern int  AcbAPI_initialize(long acbId, int playerType, const char *appId,
                              void (*cb)(long, long, long, long, long,
                                         const char *));
extern int  AcbAPI_setSinkType(long acbId, int sinkType);
extern int  AcbAPI_setMediaId(long acbId, const char *connId);
/* 3-arg ABI CONFIRMED by crashd: ACB::AcbCore::createTask(TaskType, long*)
 * writes the task id through arg3 — 2-arg calls leave garbage in r2 and
 * segfault (or silently corrupt memory when r2 happens to be writable).
 * Audio is owned by the pipeline — never feed it to ACB (causes
 * SOUND_ERROR_019), so AcbAPI_setMediaAudioData is intentionally unused. */
extern int  AcbAPI_setMediaVideoData(long acbId, const char *payload,
                                     long *taskId);
extern int  AcbAPI_setState(long acbId, int appState, int playState,
                            long *taskId);
extern int  AcbAPI_setDisplayWindow(long acbId, long x, long y, long w, long h,
                                    int fullScreen, long *taskId);
extern int  AcbAPI_finalize(long acbId);
extern void AcbAPI_destroy(long acbId);
#define PLAYER_TYPE_MSE     10
#define SINK_TYPE_MAIN      0
#define APPSTATE_FOREGROUND 1
#define PLAYSTATE_UNLOADED  0
#define PLAYSTATE_LOADED    1
#define PLAYSTATE_PLAYING   2

/* ---- LG StarfishMediaAPIs (libplayerAPIs): in-process GStreamer pipeline.
 * Buffer-feed path — the pipeline lives in OUR process, so ACB can bind its
 * video sink (unlike uMS's out-of-process URI pipeline). Called via the
 * mangled C++ symbols; `this` is an over-allocated buffer we construct in
 * place (object size unknown, so we never hand it to C++ new/delete). Methods
 * returning std::string use a hidden sret pointer (first arg); we read the
 * char* at offset 0 (SSO holds short replies like "Ok"/"BufferFull"). ---- */
extern void SMP_ctor(void *self, const char *appId)
    __asm__("_ZN17StarfishMediaAPIsC1EPKc");
extern void SMP_dtor(void *self) __asm__("_ZN17StarfishMediaAPIsD1Ev");
extern int  SMP_Load(void *self, const char *payload,
                     void (*cb)(int, long long, const char *))
    __asm__("_ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_E");
extern void SMP_Feed(void *sret, void *self, const char *payload)
    __asm__("_ZN17StarfishMediaAPIs4FeedB5cxx11EPKc");
extern int  SMP_Play(void *self) __asm__("_ZN17StarfishMediaAPIs4PlayEv");
extern int  SMP_Unload(void *self) __asm__("_ZN17StarfishMediaAPIs6UnloadEv");
extern void SMP_notifyForeground(void *self)
    __asm__("_ZN17StarfishMediaAPIs16notifyForegroundEv");
extern int  SMP_isLoadCompleted(void *self)
    __asm__("_ZN17StarfishMediaAPIs15isLoadCompletedEv");
extern int  SMP_Pause(void *self) __asm__("_ZN17StarfishMediaAPIs5PauseEv");
extern void SMP_setCurrentPlaytime(void *self, long long t)
    __asm__("_ZN17StarfishMediaAPIs18setCurrentPlaytimeEx");
extern int  SMP_flush(void *self) __asm__("_ZN17StarfishMediaAPIs5flushEv");

static unsigned char g_smp[65536] __attribute__((aligned(16)));
static int g_smpReady = 0;

static long g_acb = 0, g_taskId = 0;
static int  videoInfoSent = 0;   /* ACB setMediaVideoData sent once, after PLAYING */
static FILE *elogf = NULL;       /* shared event/diagnostic log */
/* /tmp/poc-ptype: ACB playerType for AcbAPI_initialize (default MSE=10).
 * Sweeping this is log-readable: acb_cb reports whether setMediaVideoData →
 * tv.display succeeds, so no screen look is needed to find a working type. */
static int g_ptype = PLAYER_TYPE_MSE;

static void acb_cb(long a, long t, long ev, long app, long play,
                   const char *reply) {
    (void)a; (void)t; (void)app; (void)play;
    if (elogf) {
        fprintf(elogf, "acb_cb ev=%ld reply=%s\n", ev, reply ? reply : "");
        fflush(elogf);
    }
}

/* crash tracer: log faulting PC + the /proc/self/maps line containing it, so
 * we can tell which library (libplayerAPIs, gstreamer, ours) faulted */
static void crash_handler(int sig, siginfo_t *si, void *uc) {
    unsigned long pc = 0;
    ucontext_t *c = (ucontext_t *)uc;
#if defined(__arm__)
    pc = (unsigned long)c->uc_mcontext.arm_pc;
#endif
    if (elogf) {
        fprintf(elogf, "\n*** SIGNAL %d addr=%p pc=0x%lx\n", sig,
                si ? si->si_addr : 0, pc);
        FILE *m = fopen("/proc/self/maps", "r");
        if (m) {
            char line[256];
            while (fgets(line, sizeof line, m)) {
                unsigned long lo = 0, hi = 0;
                if (sscanf(line, "%lx-%lx", &lo, &hi) == 2 &&
                    pc >= lo && pc < hi) {
                    fprintf(elogf, "in: %s", line);
                    break;
                }
            }
            fclose(m);
        }
        fflush(elogf);
    }
    _exit(3);
}

static void install_crash_tracer(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = crash_handler;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, NULL);
    sigaction(SIGABRT, &sa, NULL);
    sigaction(SIGBUS, &sa, NULL);
}

/* Create + initialize the ACB (App Common Binding) that binds the decoded video
 * sink to the display plane. We deliberately DON'T register our own
 * com.webos.media client — that collides with the uMS connection
 * StarfishMediaAPIs registers (which then fails acquire with CONN_FIND_ERR).
 * The pipeline owns that connection; we only need ACB for the plane bind. */
static int acb_init(void) {
    g_acb = AcbAPI_create();
    if (g_acb) {
        const char *appId = getenv("APPID");
        AcbAPI_initialize(g_acb, g_ptype, appId ? appId : "com.glin.plexpoc", acb_cb);
    }
    if (elogf) { fprintf(elogf, "acb create=%ld\n", g_acb); fflush(elogf); }
    return 1;
}

static void ls2_pump(void) {
    int guard = 8;
    while (guard-- && g_main_context_pending(NULL))
        g_main_context_iteration(NULL, 0);
}

/* ================= buffer-feed playback (StarfishMediaAPIs) ================= */
/* Validation build: read a raw H264 Annex-B sample from /tmp/sample.h264,
 * split into access units (each starts at an AUD: 00 00 00 01 09), and feed
 * them to an in-process pipeline while ACB binds the video plane. */
static unsigned char *bf_data = NULL;   /* whole sample in memory */
static long bf_len = 0;
static long bf_au[40000];               /* AU start offsets */
static int  bf_naus = 0, bf_next = 0, bf_loop = 0;
static int  bf_loaded = 0, bf_bound = 0, bf_playing = 0, bf_started = 0;
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
static volatile int bf_frames = 0;      /* decoded-frame events (type 0) seen */

/* ---- player UI/transport state ---- */
static volatile long long g_playpos_ns = 0;  /* displayed position (wall-clock driven) */
static long long pl_dur_ns = 0;              /* total duration (from MKV Info) */
static int       pl_paused = 0;
static int       resumePausePending = 0;     /* re-pause once a resume seek's frame is shown */
static unsigned  pl_hud_until = 0;           /* SDL ticks: HUD auto-hides after */
static long long pl_scrub_ns = -1;           /* scrub preview target (-1 = not scrubbing) */
/* Seek keeps the FED pts continuous so the pipeline never sees a jump: fed_pts =
 * real_pts + g_pts_shift. After a seek, g_rebase_pending recomputes the shift
 * from the first new AU so playback continues from the last fed pts. */
static volatile long long g_pts_shift = 0;
static long long g_max_fed_pts = 0;
static volatile int g_rebase_pending = 0;

/* Feed one AU; returns reply's first char ('O'=Ok, 'B'=BufferFull, else err) */
static char bf_feed(const unsigned char *p, unsigned size, long long pts,
                    int esData) {
    char j[160];
    snprintf(j, sizeof j,
             "{\"bufferAddr\":\"%p\",\"bufferSize\":%u,\"pts\":%lld,"
             "\"esData\":%d}", (const void *)p, size, pts, esData);
    unsigned char ret[32];
    memset(ret, 0, sizeof ret);
    SMP_Feed(ret, g_smp, j);
    char *s = *(char **)ret;             /* std::string _M_p at offset 0 */
    static int logged = 0;
    if (elogf && logged < 3) { logged++;
        fprintf(elogf, "feed reply=\"%s\"\n", s ? s : "(null)"); fflush(elogf); }
    /* reply is JSON like {"returnValue":"Ok"} or {"returnValue":"BufferFull"} */
    if (!s) return 'e';
    if (strstr(s, "BufferFull")) return 'B';
    if (strstr(s, "Ok")) return 'O';
    return 'e';
}

/* StarfishMediaAPIs Load callback: (eventType, numValue, jsonStr) */
static void starfish_cb(int type, long long num, const char *str) {
    if (elogf) {
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
static char         g_url[1024] = "";
static au_node     *bf_pending = NULL;  /* AU popped but not yet accepted (BufferFull) */
static http_stream  g_hs;
static mkv_ctx      g_mkv;
static pthread_t    g_stream_th, g_load_th;
static int          g_stream_created = 0, g_load_created = 0;
static long long          g_file_size  = 0;    /* full part size (from first GET) */
static volatile long long g_seek_byte  = -1;   /* demux thread: reposition here */
static volatile long long g_seek_to_ns = -1;   /* UI request: seek to this time */
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
    long long best = g_cues[0].byte, bestt = -1;
    for (int i = 0; i < g_ncues; i++)
        if (g_cues[i].t_ns <= t && g_cues[i].t_ns > bestt) { bestt = g_cues[i].t_ns; best = g_cues[i].byte; }
    return best;
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

static int start_bufferfeed(void) {
    /* streaming mode if /tmp/poc-url holds a PMS part URL; else the file sample */
    FILE *uf = fopen("/tmp/poc-url", "r");
    if (uf) {
        if (fgets(g_url, sizeof g_url, uf)) {
            size_t l = strlen(g_url);
            while (l && (g_url[l-1]=='\n' || g_url[l-1]=='\r' || g_url[l-1]==' ')) g_url[--l] = 0;
            if (g_url[0]) bf_stream = 1;
        }
        fclose(uf);
    }
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
        if (!g_cues_ready) {                /* preflight the Cue index (kept across app-switch) */
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
static void stop_bufferfeed(int keep_cues) {
    if (!bf_started) return;
    /* Stop the detached-no-more cue preflight FIRST and JOIN it before anything
     * frees g_cues — cue_cb writes g_cues on the preflight thread, so freeing it
     * from under a still-running thread is a use-after-free. */
    g_cues_abort = 1;
    if (bf_stream) { aq_abort(&g_aq); http_close(&g_hs); http_close(&g_hs2); }  /* unblock threads */
    if (g_cues_created)   { pthread_join(g_cues_th, NULL);   g_cues_created = 0; }
    if (g_stream_created) { pthread_join(g_stream_th, NULL); g_stream_created = 0; }
    if (g_load_created)   { pthread_join(g_load_th, NULL);   g_load_created = 0; }
    if (g_smpReady) {
        SMP_Unload(g_smp);
        if (g_acb) AcbAPI_setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_UNLOADED, &g_taskId);
        SMP_dtor(g_smp);
        g_smpReady = 0;
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
        char r = bf_feed(bf_data + off, (unsigned)(end - off), pts, 1);
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
        char r = bf_feed(bf_pending->data, (unsigned)bf_pending->len, fed_pts, bf_pending->es);
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
    SMP_ctor(g_smp, NULL);
    g_smpReady = 1;
    SMP_notifyForeground(g_smp);
    if (elogf) { fprintf(elogf, "SMP: calling Load (uid=NULL)\n"); fflush(elogf); }
    int ok = SMP_Load(g_smp, bf_payload, starfish_cb);
    if (elogf) { fprintf(elogf, "SMP: Load returned ok=%d\n", ok); fflush(elogf); }
    return NULL;
}

/* called from the main loop: once loaded, bind ACB, Play, then feed AUs */
static void bufferfeed_pump(Uint32 now) {
    if (!bf_started || !g_smpReady) return;   /* wait for media-thread ctor */
    (void)now;

    /* pending seek: flush the pipeline, drop queued AUs, and tell the demux
     * thread to re-open the HTTP stream at the byte offset for the target time. */
    if (bf_stream && g_seek_to_ns >= 0 && bf_playing && g_file_size > 0 && pl_dur_ns > 0) {
        long long t = g_seek_to_ns; g_seek_to_ns = -1;
        if (t < 0) t = 0;
        SMP_flush(g_smp);             /* drop decoded/queued frames; resets the clock to ~0 */
        SMP_setCurrentPlaytime(g_smp, 0);
        SMP_Play(g_smp);              /* resume presentation after the flush */
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

    if (!bf_loaded && SMP_isLoadCompleted(g_smp)) {
        bf_loaded = 1;
        if (elogf) { fprintf(elogf, "SMP loadCompleted\n"); fflush(elogf); }
    }
    /* NB: getMediaID() returns empty for buffer-feed — the real pipeline mediaId
     * arrives as "context" in the type-4 sourceInfo callback (see starfish_cb).
     * Polling getMediaID here is useless AND crashes under demux-thread contention
     * (SIGSEGV inside libplayerAPIs), so it's removed. */
    /* Play as soon as loaded so the pipeline decodes fed frames */
    if (bf_loaded && !bf_playing) {
        SMP_Play(g_smp);
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
        AcbAPI_setSinkType(g_acb, SINK_TYPE_MAIN);
        AcbAPI_setMediaId(g_acb, bf_mediaId);
        AcbAPI_setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_LOADED, &g_taskId);
        if (elogf) { fprintf(elogf, "SMP ACB bound id=%s\n", bf_mediaId); fflush(elogf); }
    }
    /* Send the WHOLE sourceInfo envelope (context + content + video) VERBATIM,
     * once the decoder is producing frames so the videosink is bound to MAIN.
     * The envelope's "context" is our pipeline id (= the VSM MAIN-sink context),
     * so tv.display resolves the video and every video.* field parses correctly.
     * Then window + PLAYING (VSM connect already auto-unmuted the plane). */
    if (bf_bound && !videoInfoSent && sourceInfoRaw[0] && bf_frames >= 2) {
        int rv = AcbAPI_setMediaVideoData(g_acb, sourceInfoRaw, &g_taskId);
        if (elogf) { fprintf(elogf, "setMediaVideoData rv=%d frames=%d payload=%.240s\n",
                             rv, bf_frames, sourceInfoRaw); fflush(elogf); }
        if (rv != -1) {   /* -1 = client-side isJsonError reject; else accepted */
            videoInfoSent = 1;
            AcbAPI_setDisplayWindow(g_acb, 0, 0, 1920, 1080, 1, &g_taskId);
            AcbAPI_setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_PLAYING, &g_taskId);
            if (elogf) { fprintf(elogf, "setMediaVideoData sent → window+PLAYING\n"); fflush(elogf); }
        }
    }
    /* feed AUs once playing (Feed only succeeds after Play) */
    if (bf_stream && g_mkv.duration_ns > 0) pl_dur_ns = g_mkv.duration_ns;
    if (bf_playing && !pl_paused) { if (bf_stream) bf_feed_stream(); else bf_feed_ahead(); }
}

#define SCR_W 1920
#define SCR_H 1080

#define ROWS 5
#define COLS 10
#define CARD_W 420.0f
#define CARD_H 236.0f
#define GAP 36.0f
#define MARGIN_X 90.0f
#define ROW_TITLE_H 30.0f
#define ROW_PITCH (CARD_H + ROW_TITLE_H + 54.0f)
#define CONTENT_Y 200.0f
#define GLOW_PAD 48.0f /* extra quad space around card for glow/shadow */

static const char *VS_SRC =
    "attribute vec2 a_pos;\n"
    "uniform vec4 u_rect;\n"   /* x,y,w,h in screen px */
    "uniform vec2 u_screen;\n"
    "varying vec2 v_uv;\n"
    "void main(){\n"
    "  v_uv = a_pos;\n"
    "  vec2 px = u_rect.xy + a_pos * u_rect.zw;\n"
    "  vec2 ndc = px / u_screen * 2.0 - 1.0;\n"
    "  gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);\n"
    "}\n";

static const char *FS_SRC =
    "precision mediump float;\n"
    "varying vec2 v_uv;\n"
    "uniform vec2 u_size;\n"    /* quad size px */
    "uniform float u_pad;\n"    /* inset from quad edge to card edge */
    "uniform float u_radius;\n"
    "uniform vec4 u_colTop;\n"
    "uniform vec4 u_colBot;\n"
    "uniform float u_focus;\n"  /* 0..1 focus ring+glow */
    "uniform float u_shape;\n"  /* 0 rounded rect, 1 right-pointing triangle */
    "uniform float u_radR;\n"   /* right-corner radius (u_radius = left) */
    "float sdBox(vec2 p, vec2 b, float r){\n"
    "  vec2 q = abs(p) - b + vec2(r);\n"
    "  return length(max(q,0.0)) + min(max(q.x,q.y),0.0) - r;\n"
    "}\n"
    "void main(){\n"
    "  if (u_shape > 0.5) {\n"                       /* play triangle in the quad */
    "    float tri = step(0.5*v_uv.x, v_uv.y) * step(v_uv.y, 1.0 - 0.5*v_uv.x);\n"
    "    gl_FragColor = vec4(u_colTop.rgb * tri, tri * u_colTop.a);\n"
    "    return;\n"
    "  }\n"
    "  vec2 p = (v_uv - 0.5) * u_size;\n"
    "  vec2 hsz = u_size * 0.5 - vec2(u_pad);\n"
    "  float rad = (p.x > 0.0) ? u_radR : u_radius;\n"
    "  float d = sdBox(p, hsz, rad);\n"
    "  vec4 fill = mix(u_colTop, u_colBot, v_uv.y);\n"
    "  float aFill = 1.0 - smoothstep(-1.0, 1.0, d);\n"
    "  vec3 rgb = fill.rgb * aFill;\n"
    "  float a = aFill * fill.a;\n"
    "  if (u_focus > 0.001) {\n"
    "    float ring = (1.0 - smoothstep(1.5, 4.0, abs(d - 5.0))) * u_focus;\n"
    "    float glow = exp(-max(d, 0.0) / 14.0) * 0.40 * u_focus * step(0.0, d);\n"
    "    rgb += vec3(1.0) * ring + vec3(0.85, 0.9, 1.0) * glow;\n"
    "    a = max(a, max(ring, glow));\n"
    "  }\n"
    "  gl_FragColor = vec4(rgb, a);\n"
    "}\n";

static GLuint prog;
static GLint loc_rect, loc_screen, loc_size, loc_pad, loc_radius,
             loc_colTop, loc_colBot, loc_focus, loc_shape, loc_radR;

static GLuint compile(GLenum type, const char *src) {
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &src, NULL);
    glCompileShader(s);
    GLint ok = 0;
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[1024];
        glGetShaderInfoLog(s, sizeof log, NULL, log);
        fprintf(stderr, "shader error: %s\n", log);
        exit(1);
    }
    return s;
}

static void init_gl(void) {
    prog = glCreateProgram();
    glAttachShader(prog, compile(GL_VERTEX_SHADER, VS_SRC));
    glAttachShader(prog, compile(GL_FRAGMENT_SHADER, FS_SRC));
    glBindAttribLocation(prog, 0, "a_pos");
    glLinkProgram(prog);
    GLint ok = 0;
    glGetProgramiv(prog, GL_LINK_STATUS, &ok);
    if (!ok) { fprintf(stderr, "link failed\n"); exit(1); }
    glUseProgram(prog);
    loc_rect   = glGetUniformLocation(prog, "u_rect");
    loc_screen = glGetUniformLocation(prog, "u_screen");
    loc_size   = glGetUniformLocation(prog, "u_size");
    loc_pad    = glGetUniformLocation(prog, "u_pad");
    loc_radius = glGetUniformLocation(prog, "u_radius");
    loc_colTop = glGetUniformLocation(prog, "u_colTop");
    loc_colBot = glGetUniformLocation(prog, "u_colBot");
    loc_focus  = glGetUniformLocation(prog, "u_focus");
    loc_shape  = glGetUniformLocation(prog, "u_shape");
    loc_radR   = glGetUniformLocation(prog, "u_radR");
    glUniform2f(loc_screen, (float)SCR_W, (float)SCR_H);

    static const GLfloat quad[8] = {0,0, 1,0, 0,1, 1,1};
    GLuint vbo;
    glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof quad, quad, GL_STATIC_DRAW);
    glEnableVertexAttribArray(0);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, 0);

    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
}

static void draw_rect(float x, float y, float w, float h, float pad,
                      float radius, const float top[4], const float bot[4],
                      float focus) {
    glUniform4f(loc_rect, x, y, w, h);
    glUniform2f(loc_size, w, h);
    glUniform1f(loc_pad, pad);
    glUniform1f(loc_radius, radius);
    glUniform1f(loc_radR, radius);
    glUniform4fv(loc_colTop, 1, top);
    glUniform4fv(loc_colBot, 1, bot);
    glUniform1f(loc_focus, focus);
    glUniform1f(loc_shape, 0.0f);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

/* solid rect with independent left/right corner radii (radL, radR) */
static void draw_rrect(float x, float y, float w, float h, float radL,
                       float radR, const float col[4]) {
    glUniform4f(loc_rect, x, y, w, h);
    glUniform2f(loc_size, w, h);
    glUniform1f(loc_pad, 0.0f);
    glUniform1f(loc_radius, radL);
    glUniform1f(loc_radR, radR);
    glUniform4fv(loc_colTop, 1, col);
    glUniform4fv(loc_colBot, 1, col);
    glUniform1f(loc_focus, 0.0f);
    glUniform1f(loc_shape, 0.0f);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

/* filled right-pointing play triangle inscribed in the given box */
static void draw_ptri(float x, float y, float w, float h, const float col[4]) {
    glUniform4f(loc_rect, x, y, w, h);
    glUniform2f(loc_size, w, h);
    glUniform4fv(loc_colTop, 1, col);
    glUniform4fv(loc_colBot, 1, col);
    glUniform1f(loc_focus, 0.0f);
    glUniform1f(loc_shape, 1.0f);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

/* ---- text rendering: SDL2_ttf → GL textures, cached by (string,size) ---- */
static const char *VS_TEXT =
    "attribute vec2 a_pos;\n"
    "uniform vec4 u_trect;\n"
    "uniform vec2 u_tscreen;\n"
    "varying vec2 v_tuv;\n"
    "void main(){ v_tuv=a_pos; vec2 px=u_trect.xy+a_pos*u_trect.zw;\n"
    "  vec2 ndc=px/u_tscreen*2.0-1.0; gl_Position=vec4(ndc.x,-ndc.y,0.0,1.0); }\n";
static const char *FS_TEXT =
    "precision mediump float;\n"
    "varying vec2 v_tuv;\n"
    "uniform sampler2D u_tex;\n"
    "uniform vec4 u_tcol;\n"       /* text color; texture alpha = glyph coverage */
    "void main(){ float a=texture2D(u_tex,v_tuv).a; gl_FragColor=vec4(u_tcol.rgb, u_tcol.a*a); }\n";

static GLuint tprog;
static GLint  tl_rect, tl_screen, tl_col, tl_tex;
static TTF_Font *g_fonts[80], *g_fonts_b[80];   /* regular / synthesized-bold per ptsize */
static int g_text_ok = 0;

#define APPDIR_PATH "/media/developer/apps/usr/palm/applications/com.glin.plexpoc/"
#define APP_FONT      APPDIR_PATH "appfont.ttf"        /* Arial Regular */
#define APP_FONT_BOLD APPDIR_PATH "appfont-bold.ttf"   /* Arial Bold (real face) */
static TTF_Font *font_at(int sz, int bold) {
    if (sz < 8) sz = 8; if (sz > 79) sz = 79;
    TTF_Font **arr = bold ? g_fonts_b : g_fonts;
    if (!arr[sz]) {
        arr[sz] = TTF_OpenFont(bold ? APP_FONT_BOLD : APP_FONT, sz);
        if (!arr[sz]) {   /* fallbacks: regular app font, then DroidSans */
            arr[sz] = TTF_OpenFont(APP_FONT, sz);
            if (!arr[sz]) arr[sz] = TTF_OpenFont("/usr/share/fonts/DroidSans.ttf", sz);
            if (arr[sz] && bold) TTF_SetFontStyle(arr[sz], 0x01);
        }
    }
    return arr[sz];
}
static void init_text(void) {
    if (TTF_Init() != 0) { if (elogf){fprintf(elogf,"TTF_Init failed\n");fflush(elogf);} return; }
    tprog = glCreateProgram();
    glAttachShader(tprog, compile(GL_VERTEX_SHADER, VS_TEXT));
    glAttachShader(tprog, compile(GL_FRAGMENT_SHADER, FS_TEXT));
    glBindAttribLocation(tprog, 0, "a_pos");
    glLinkProgram(tprog);
    GLint ok = 0; glGetProgramiv(tprog, GL_LINK_STATUS, &ok);
    if (!ok) { if (elogf){fprintf(elogf,"text prog link failed\n");fflush(elogf);} return; }
    tl_rect   = glGetUniformLocation(tprog, "u_trect");
    tl_screen = glGetUniformLocation(tprog, "u_tscreen");
    tl_col    = glGetUniformLocation(tprog, "u_tcol");
    tl_tex    = glGetUniformLocation(tprog, "u_tex");
    if (font_at(28, 0)) g_text_ok = 1;
    glUseProgram(prog);
    if (elogf) { fprintf(elogf, "init_text ok=%d\n", g_text_ok); fflush(elogf); }
}

#define TCACHE 48
static struct { char s[96]; int sz; int bold; GLuint tex; int w, h; unsigned use; } tcache[TCACHE];
static unsigned tclock = 0;
/* returns GL texture id (0 on failure) and sets w,h out-params */
static GLuint text_tex(const char *s, int sz, int bold, int *w, int *h) {
    for (int i = 0; i < TCACHE; i++)
        if (tcache[i].tex && tcache[i].sz == sz && tcache[i].bold == bold &&
            strcmp(tcache[i].s, s) == 0) {
            tcache[i].use = ++tclock; *w = tcache[i].w; *h = tcache[i].h; return tcache[i].tex; }
    TTF_Font *f = font_at(sz, bold); if (!f) return 0;
    SDL_Color white = {255, 255, 255, 255};
    SDL_Surface *surf = TTF_RenderUTF8_Blended(f, s, white);
    if (!surf) return 0;
    GLuint tex; glGenTextures(1, &tex); glBindTexture(GL_TEXTURE_2D, tex);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, surf->w, surf->h, 0,
                 GL_RGBA, GL_UNSIGNED_BYTE, surf->pixels);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    int sw = surf->w, sh = surf->h;
    SDL_FreeSurface(surf);
    int slot = 0; unsigned oldest = ~0u;
    for (int i = 0; i < TCACHE; i++) {
        if (!tcache[i].tex) { slot = i; break; }
        if (tcache[i].use < oldest) { oldest = tcache[i].use; slot = i; }
    }
    if (tcache[slot].tex) glDeleteTextures(1, &tcache[slot].tex);
    strncpy(tcache[slot].s, s, sizeof tcache[slot].s - 1);
    tcache[slot].s[sizeof tcache[slot].s - 1] = 0;
    tcache[slot].sz = sz; tcache[slot].bold = bold;
    tcache[slot].tex = tex; tcache[slot].w = sw; tcache[slot].h = sh;
    tcache[slot].use = ++tclock;
    *w = sw; *h = sh; return tex;
}
/* align: 0 left, 1 center, 2 right (x is the anchor edge). returns text width. */
static float draw_text(const char *s, float x, float y, int sz,
                       const float col[4], int align, int bold) {
    if (!g_text_ok || !s || !s[0]) return 0;
    int w = 0, h = 0; GLuint tex = text_tex(s, sz, bold, &w, &h);
    if (!tex) return 0;
    float dx = align == 1 ? x - w * 0.5f : align == 2 ? x - w : x;
    glUseProgram(tprog);
    glUniform2f(tl_screen, (float)SCR_W, (float)SCR_H);
    glUniform4fv(tl_col, 1, col);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, tex);
    glUniform1i(tl_tex, 0);
    glUniform4f(tl_rect, dx, y, (float)w, (float)h);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    glUseProgram(prog);   /* restore rect program for subsequent draw_rect */
    return (float)w;
}

static void hsv(float h, float s, float v, float out[4]) {
    float c = v * s, hp = fmodf(h, 360.0f) / 60.0f;
    float x = c * (1.0f - fabsf(fmodf(hp, 2.0f) - 1.0f));
    float r = 0, g = 0, b = 0;
    if (hp < 1)      { r = c; g = x; }
    else if (hp < 2) { r = x; g = c; }
    else if (hp < 3) { g = c; b = x; }
    else if (hp < 4) { g = x; b = c; }
    else if (hp < 5) { r = x; b = c; }
    else             { r = c; b = x; }
    float m = v - c;
    out[0] = r + m; out[1] = g + m; out[2] = b + m; out[3] = 1.0f;
}

/* critically-damped spring step */
static void spring(float *pos, float *vel, float target, float k, float dt) {
    float c = 2.0f * sqrtf(k);
    float a = k * (target - *pos) - c * (*vel);
    *vel += a * dt;
    *pos += *vel * dt;
}

/* --- seven-segment FPS digits (quads) --- */
static const unsigned char SEG[10] = {
    0x3F, 0x06, 0x5B, 0x4F, 0x66, 0x6D, 0x7D, 0x07, 0x7F, 0x6F};

static void draw_digit(int d, float x, float y, float s, const float col[4]) {
    /* segments: 0 top,1 tr,2 br,3 bottom,4 bl,5 tl,6 mid */
    float w = 0.16f * s;
    struct { float x, y, w, h; } g[7] = {
        {0, 0, 1.0f, 0},       {1.0f, 0, 0, 0.5f}, {1.0f, 0.5f, 0, 0.5f},
        {0, 1.0f, 1.0f, 0},    {0, 0.5f, 0, 0.5f}, {0, 0, 0, 0.5f},
        {0, 0.5f, 1.0f, 0}};
    for (int i = 0; i < 7; i++) {
        if (!(SEG[d] >> i & 1)) continue;
        float sx = x + g[i].x * s - w / 2, sy = y + g[i].y * s - w / 2;
        float sw = g[i].w * s + w, sh = g[i].h * s + w;
        draw_rect(sx, sy, sw, sh, 2.0f, (w + 4) / 2 - 2, col, col, 0);
    }
}

static void draw_number(int n, float right_x, float y, float s,
                        const float col[4]) {
    if (n < 0) n = 0;
    if (n > 999) n = 999;
    float adv = s + 0.55f * s;
    float x = right_x - adv;
    do {
        draw_digit(n % 10, x, y, s, col);
        n /= 10;
        x -= adv;
    } while (n > 0);
}

/* ---- playback HUD (Apple TV-style) ---- */
static char g_title[128]   = "Frozen";               /* TODO: pull from PMS metadata */
static char g_ctxline[96]  = "2013 \xc2\xb7 PG \xc2\xb7 1h 42m";  /* context line (UTF-8 middot) */

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
static float text_w(const char *s, int sz, int bold) {
    if (!g_text_ok || !s || !s[0]) return 0;
    int w = 0, h = 0; text_tex(s, sz, bold, &w, &h); return (float)w;
}
/* rounded translucent pill with a bold label; returns pill width */
static float draw_pill(const char *label, float x, float y, int focused) {
    int sz = 26; float tw = text_w(label, sz, 1);
    float padx = 30, h = 56, w = tw + 2 * padx;
    float bg[4], fg[4];
    if (focused) { bg[0]=0.96f;bg[1]=0.96f;bg[2]=0.98f;bg[3]=0.97f; fg[0]=0.06f;fg[1]=0.07f;fg[2]=0.09f;fg[3]=1; }
    else         { bg[0]=1;bg[1]=1;bg[2]=1;bg[3]=0.13f;             fg[0]=0.94f;fg[1]=0.95f;fg[2]=1;fg[3]=1; }
    draw_rect(x, y, w, h, 0, h * 0.5f, bg, bg, 0);
    draw_text(label, x + w * 0.5f, y + 13, sz, fg, 1, 1);
    return w;
}
static void draw_hud(void) {
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

int main(int argc, char **argv) {
    (void)argc; (void)argv;
    elogf = fopen("/tmp/poc-events.log", "w");
    freopen("/tmp/poc-stderr.log", "w", stderr); /* capture abort/assert text */
    install_crash_tracer();
    {
        FILE *pf = fopen("/tmp/poc-ptype", "r");   /* dev: ACB playerType override */
        if (pf) { fscanf(pf, "%d", &g_ptype); fclose(pf); }
        if (elogf) { fprintf(elogf, "ptype=%d\n", g_ptype); fflush(elogf); }
    }
    SDL_SetMainReady();
    SDL_SetHint(SDL_HINT_VIDEO_ALLOW_SCREENSAVER, "0");
    /* request BACK key delivery from the webOS access policy */
    setenv("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true", 1);
    if (SDL_Init(SDL_INIT_VIDEO) != 0) {
        fprintf(stderr, "SDL_Init: %s\n", SDL_GetError());
        return 1;
    }
    fprintf(stderr, "video driver: %s\n", SDL_GetCurrentVideoDriver());

    SDL_GL_SetAttribute(SDL_GL_CONTEXT_PROFILE_MASK, SDL_GL_CONTEXT_PROFILE_ES);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MAJOR_VERSION, 2);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MINOR_VERSION, 0);
    /* per-pixel alpha so the video plane (behind the GUI) can show through:
     * force a full 32-bit RGBA config (webOS EGL otherwise hands back an
     * opaque XRGB window buffer the compositor won't alpha-blend) */
    SDL_GL_SetAttribute(SDL_GL_RED_SIZE, 8);
    SDL_GL_SetAttribute(SDL_GL_GREEN_SIZE, 8);
    SDL_GL_SetAttribute(SDL_GL_BLUE_SIZE, 8);
    SDL_GL_SetAttribute(SDL_GL_ALPHA_SIZE, 8);
    SDL_GL_SetAttribute(SDL_GL_BUFFER_SIZE, 32);
    SDL_Window *win = SDL_CreateWindow("plexpoc", 0, 0, SCR_W, SCR_H,
                                       SDL_WINDOW_OPENGL | SDL_WINDOW_FULLSCREEN);
    if (!win) {
        fprintf(stderr, "CreateWindow: %s\n", SDL_GetError());
        return 1;
    }
    SDL_GLContext ctx = SDL_GL_CreateContext(win);
    if (!ctx) {
        fprintf(stderr, "GL ctx: %s\n", SDL_GetError());
        return 1;
    }
    SDL_GL_SetSwapInterval(1);
    fprintf(stderr, "GL: %s / %s\n", glGetString(GL_RENDERER),
            glGetString(GL_VERSION));

    /* grab the wayland surface/display and make it non-opaque */
    {
        SDL_SysWMinfo wm;
        SDL_VERSION(&wm.version);
        int a = -1;
        SDL_GL_GetAttribute(SDL_GL_ALPHA_SIZE, &a);
        GLint abits = -1, rbits = -1;
        glGetIntegerv(GL_ALPHA_BITS, &abits);
        glGetIntegerv(GL_RED_BITS, &rbits);
        if (elogf) {
            fprintf(elogf, "FB bits: alpha=%d red=%d (config alpha=%d)\n",
                    abits, rbits, a);
            fflush(elogf);
        }
        if (SDL_GetWindowWMInfo(win, &wm)) {
            /* the info union's wayland struct is {wl_display*, wl_surface*,
             * wl_shell_surface*}; all union members share offset 0, so read
             * the first two pointers directly (header-version independent) */
            void **p = (void **)&wm.info;
            g_wl_display = p[0];
            g_wl_surface = p[1];
        }
        if (elogf) {
            fprintf(elogf, "wm subsys=%d wl_surface=%p wl_display=%p alpha=%d\n",
                    wm.subsystem, g_wl_surface, g_wl_display, a);
            fflush(elogf);
        }
        clear_opaque_region();
    }
    init_gl();
    init_text();

    /* card colors */
    static float colTop[ROWS][COLS][4], colBot[ROWS][COLS][4];
    for (int r = 0; r < ROWS; r++)
        for (int c = 0; c < COLS; c++) {
            float h = (float)((r * 67 + c * 31) % 360);
            hsv(h, 0.55f, 0.50f, colTop[r][c]);
            hsv(h + 18.0f, 0.65f, 0.28f, colBot[r][c]);
        }

    int fr = 0, fc = 0;             /* focused row/col */
    static float scale[ROWS][COLS], scaleV[ROWS][COLS];
    float scrollX[ROWS] = {0}, scrollXV[ROWS] = {0};
    float scrollY = 0, scrollYV = 0;
    for (int r = 0; r < ROWS; r++)
        for (int c = 0; c < COLS; c++) scale[r][c] = 1.0f;

    Uint32 lastInput = SDL_GetTicks(), lastAuto = 0;
    int autodir = 1;
    Uint32 t0 = SDL_GetTicks(), fpsT = t0;
    int frames = 0, fpsShown = 0;
    float bgPhase = 0;
    int running = 1;

    acb_init();
    int demo = (argc > 1 && strstr(argv[1], "demo") != NULL);
    unsigned heldSym = 0;          /* client-side key repeat (wayland) */
    Uint32 heldSince = 0, lastRep = 0, scrubLast = 0, scrubT = 0;
    int scrubDir = 0;              /* -1/+1 while scrubbing with LEFT/RIGHT held */
    int bgWasPlaying = 0;          /* backgrounded mid-playback → reload on return */
    int bgWasPaused = 0;           /* was paused when backgrounded → re-pause after resume */
    long long bgPos = 0;           /* saved position to resume from (resumePausePending is a global) */
    /* cursor visibility is SYSTEM-owned on webOS: LSM shows it on remote
     * motion and auto-hides it after idle (keycode 0x1e4 notifies us).
     * SDL_ShowCursor(DISABLE) is a one-way trap: once hidden, pointer
     * motion events stop, so nothing can ever re-enable it. Hands off.
     *
     * Instead, arbitrate in software: a D-pad press enters DPAD mode where
     * hover is ignored (button presses physically wobble the remote and
     * spray motion events); only deliberate pointer movement (accumulated
     * distance) returns control to the pointer. */
    int dpadMode = 0, ptrDrag = 0;   /* ptrDrag: dragging the scrubber with the pointer */
    float motAccum = 0, prevMx = -1, prevMy = -1;
    Uint32 lastPtrMotion = 0; int curHidden = 0;   /* auto-hide the pointer when idle in playback */
    int playing = 0;

/* vertical move keeps VISUAL alignment: pick the card under the focused
 * one given both rows' scroll offsets (Apple TV behavior) */
#define VERT_MOVE(dir) do {                                            \
        int nr = fr + (dir);                                           \
        float cx = MARGIN_X + fc * (CARD_W + GAP) - scrollX[fr]        \
                   + CARD_W * 0.5f;                                    \
        int nc = (int)((cx - MARGIN_X - CARD_W * 0.5f + scrollX[nr])   \
                       / (CARD_W + GAP) + 0.5f);                       \
        if (nc < 0) nc = 0;                                            \
        if (nc > COLS - 1) nc = COLS - 1;                              \
        fr = nr; fc = nc;                                              \
    } while (0)

#define MOVE_FOCUS(s) do {                                             \
        if ((s) == (unsigned)SDLK_LEFT && fc > 0) fc--;                \
        else if ((s) == (unsigned)SDLK_RIGHT && fc < COLS - 1) fc++;   \
        else if ((s) == (unsigned)SDLK_UP && fr > 0) VERT_MOVE(-1);    \
        else if ((s) == (unsigned)SDLK_DOWN && fr < ROWS - 1)          \
            VERT_MOVE(1);                                              \
    } while (0)

    while (running) {
        ls2_pump();
        SDL_Event e;
        while (SDL_PollEvent(&e)) {
            if (elogf && (e.type == SDL_KEYDOWN || e.type == SDL_KEYUP)) {
                const unsigned char *raw = (const unsigned char *)&e;
                fprintf(elogf, "[%u] key type=0x%x sym=0x%x scan=0x%x raw=",
                        SDL_GetTicks(), e.type, e.key.keysym.sym,
                        e.key.keysym.scancode);
                for (int bi = 0; bi < 32; bi++) fprintf(elogf, "%02x", raw[bi]);
                fprintf(elogf, "\n");
                fflush(elogf);
            }
            if (e.type == SDL_QUIT) running = 0;
            /* ---- app background/foreground (LG SDL: SDL_APP_* 0x103..0x106) ----
             * Verified on-device: switching to a full-screen app fires 0x103, the
             * media server releases our pipeline; returning fires 0x105/0x106. */
            else if (e.type == 0x103 || e.type == 0x104) {   /* WILL/DID ENTER BACKGROUND */
                if (elogf) { fprintf(elogf, "LIFECYCLE: background (playing=%d)\n", playing); fflush(elogf); }
                if (playing && !bgWasPlaying) {   /* tear down: system will release the pipeline */
                    bgPos = g_playpos_ns; bgWasPlaying = 1; bgWasPaused = pl_paused;
                    /* a held D-pad scrub / pointer drag would otherwise commit a stale
                     * seek (pl_scrub_ns==-1) on the trailing key-up after resume and
                     * clobber the accurate resume seek — cancel it now */
                    scrubDir = 0; ptrDrag = 0; pl_scrub_ns = -1;
                    stop_bufferfeed(1); playing = 0;   /* keep cues → accurate resume seek */
                }
            }
            else if (e.type == 0x105 || e.type == 0x106) {   /* WILL/DID ENTER FOREGROUND */
                if (elogf) { fprintf(elogf, "LIFECYCLE: foreground (wasPlaying=%d)\n", bgWasPlaying); fflush(elogf); }
                if (bgWasPlaying && e.type == 0x106) {        /* reload + resume on DID-enter */
                    playing = start_bufferfeed();
                    if (playing) { g_seek_to_ns = bgPos; pl_hud_until = SDL_GetTicks() + 4500;
                                   resumePausePending = bgWasPaused; }
                    bgWasPlaying = 0;
                }
            }
            else if (e.type == SDL_KEYDOWN || e.type == SDL_KEYUP) {
                /* LG's SDL fork inserts an extra 32-bit field after
                 * windowID, shifting SDL_KeyboardEvent: read the real
                 * fields at their actual offsets.
                 *   +16 state (u32), +20 scancode (u32), +24 sym (u32) */
                const unsigned char *raw = (const unsigned char *)&e;
                unsigned state, wcode, sym;
                memcpy(&state, raw + 16, 4);
                memcpy(&wcode, raw + 20, 4);
                memcpy(&sym, raw + 24, 4);
                /* raw state: low byte = pressed(1)/released(0), 0x100 bit = auto-repeat */
                int isnav = (sym == (unsigned)SDLK_LEFT || sym == (unsigned)SDLK_RIGHT ||
                             sym == 417 || wcode == 417 || sym == 412 || wcode == 412);
                if ((state & 0xff) != 1) {   /* real key-up → commit the scrub as a seek */
                    if (sym == heldSym) heldSym = 0;
                    if (playing && scrubDir != 0 && isnav) {
                        g_seek_to_ns = pl_scrub_ns; pl_scrub_ns = -1; scrubDir = 0; scrubT = 0;
                    }
                    continue;
                }
                if (state & 0x100) {         /* auto-repeat: key still held → keep scrub alive */
                    if (playing && scrubDir != 0 && isnav) scrubLast = SDL_GetTicks();
                    continue;                /* don't re-fire first-press handlers */
                }
                lastInput = SDL_GetTicks();
                if (!playing &&
                    (sym == (unsigned)SDLK_LEFT || sym == (unsigned)SDLK_RIGHT ||
                     sym == (unsigned)SDLK_UP || sym == (unsigned)SDLK_DOWN)) {
                    if (!dpadMode) SDL_webOSCursorVisibility(0);
                    dpadMode = 1;
                    motAccum = 0;
                    MOVE_FOCUS(sym);
                    heldSym = sym;
                    heldSince = lastInput;
                    lastRep = lastInput;
                }
                else if (wcode == 0x1e4) /* LG: pointer auto-hidden; ignore */
                    ;
                else if (sym == (unsigned)SDLK_RETURN ||
                         sym == (unsigned)SDLK_KP_ENTER ||
                         sym == (unsigned)SDLK_SELECT) {
                    if (!playing) {
                        /* OK on a shelf card → start the demo movie stream */
                        playing = start_bufferfeed();
                        pl_paused = 0;
                        pl_hud_until = lastInput + 4500;
                        if (!dpadMode) { SDL_webOSCursorVisibility(0); dpadMode = 1; }
                    } else {
                        /* OK during playback → toggle play/pause */
                        pl_paused = !pl_paused;
                        if (g_smpReady) { if (pl_paused) SMP_Pause(g_smp); else SMP_Play(g_smp); }
                        pl_hud_until = lastInput + 4500;
                    }
                }
                /* dedicated Magic Remote play/pause button (this remote sends the
                 * state-appropriate key: PAUSE=wcode 72 while playing, PLAY=wcode 450
                 * while paused/stopped). Verified from the raw key log. */
                else if (wcode == 72 || sym == 415 || wcode == 415) {          /* PAUSE */
                    if (playing && !pl_paused) { pl_paused = 1; if (g_smpReady) SMP_Pause(g_smp); }
                    pl_hud_until = lastInput + 4500;
                }
                else if (wcode == 450 || sym == 19 || wcode == 19 ||
                         sym == 402 || wcode == 402) {                          /* PLAY */
                    if (!playing) {
                        playing = start_bufferfeed(); pl_paused = 0;
                        if (!dpadMode) { SDL_webOSCursorVisibility(0); dpadMode = 1; }
                    } else if (pl_paused) { pl_paused = 0; if (g_smpReady) SMP_Play(g_smp); }
                    pl_hud_until = lastInput + 4500;
                }
                else if (playing && (sym == 413 || wcode == 413)) {   /* Stop key */
                    stop_bufferfeed(0); playing = 0;
                }
                else if (playing &&
                         (sym == (unsigned)SDLK_LEFT || sym == (unsigned)SDLK_RIGHT ||
                          sym == (unsigned)SDLK_UP || sym == (unsigned)SDLK_DOWN ||
                          sym == 417 || wcode == 417 || sym == 412 || wcode == 412)) {
                    pl_hud_until = lastInput + 4500;
                    if (!curHidden) { SDL_webOSCursorVisibility(0); curHidden = 1; }  /* D-pad hides pointer */
                    if (ptrDrag) { ptrDrag = 0; pl_scrub_ns = -1; }   /* D-pad cancels a pointer drag */
                    int fwd  = (sym == (unsigned)SDLK_RIGHT || sym == 417 || wcode == 417);
                    int back = (sym == (unsigned)SDLK_LEFT  || sym == 412 || wcode == 412);
                    if ((fwd || back) && pl_dur_ns > 0) {
                        /* start a scrub PREVIEW; the main loop advances it at a
                         * steady rate while held and commits when presses stop. */
                        if (pl_scrub_ns < 0) pl_scrub_ns = g_playpos_ns;
                        pl_scrub_ns += (fwd ? 10LL : -10LL) * 1000000000LL;  /* a tap = ±10s */
                        long long cap = pl_dur_ns - 3LL * 1000000000LL;
                        if (pl_scrub_ns < 0) pl_scrub_ns = 0;
                        if (cap > 0 && pl_scrub_ns > cap) pl_scrub_ns = cap;
                        scrubDir = fwd ? 1 : -1;
                        scrubLast = lastInput;
                    }
                }
                else if (sym == (unsigned)SDLK_ESCAPE || sym == 'q' ||
                         wcode == 461 /* webOS BACK */) {
                    if (playing) { stop_bufferfeed(0); playing = 0; }
                    else running = 0;
                }
            }
            else if (e.type == SDL_MOUSEMOTION) {
                /* Magic Remote pointer: hover focuses the card under it */
                lastInput = SDL_GetTicks();
                lastPtrMotion = lastInput; curHidden = 0;   /* pointer moved → it's showing */
                float mx = (float)e.motion.x, my = (float)e.motion.y;
                if (prevMx >= 0)
                    motAccum += fabsf(mx - prevMx) + fabsf(my - prevMy);
                prevMx = mx; prevMy = my;
                if (playing) {              /* pointer wakes HUD; drag updates the scrub */
                    pl_hud_until = lastInput + 4500;
                    if (ptrDrag && pl_dur_ns > 0) {
                        float sbx = 90, sbw = (float)SCR_W - 180;
                        double frac = (mx - sbx) / sbw;
                        if (frac < 0) frac = 0; if (frac > 1) frac = 1;
                        pl_scrub_ns = (long long)(frac * (double)pl_dur_ns);
                        scrubLast = lastInput;
                    }
                    continue;
                }
                if (dpadMode) {
                    /* ignore wobble; a deliberate wave re-engages pointer */
                    if (motAccum < 120.0f) continue;
                    dpadMode = 0;
                }
                for (int r = 0; r < ROWS; r++) {
                    float rowY = CONTENT_Y + r * ROW_PITCH - scrollY +
                                 ROW_TITLE_H + 18;
                    if (my < rowY || my > rowY + CARD_H) continue;
                    for (int c = 0; c < COLS; c++) {
                        float x = MARGIN_X + c * (CARD_W + GAP) - scrollX[r];
                        if (mx >= x && mx <= x + CARD_W) { fr = r; fc = c; }
                    }
                }
            }
            else if (e.type == SDL_MOUSEBUTTONDOWN) {
                /* Magic Remote center-click (arrives as a mouse click when the
                 * pointer is active): on the scrubber → seek; elsewhere → play/pause
                 * (so the center button still works while the pointer is showing). */
                lastInput = SDL_GetTicks();
                if (playing) {
                    float cx = (float)e.button.x, cy = (float)e.button.y;
                    float sbx = 90, sbw = (float)SCR_W - 180;
                    int on_scrub = (pl_dur_ns > 0 && cy > SCR_H - 270 && cy < SCR_H - 110 &&
                                    cx >= sbx && cx <= sbx + sbw);
                    if (on_scrub) {              /* start a drag; commit on button-up */
                        double frac = (cx - sbx) / sbw;
                        if (frac < 0) frac = 0; if (frac > 1) frac = 1;
                        long long t = (long long)(frac * (double)pl_dur_ns);
                        long long cap = pl_dur_ns - 3LL * 1000000000LL;
                        if (cap > 0 && t > cap) t = cap;
                        pl_scrub_ns = t; ptrDrag = 1; scrubLast = lastInput;
                    } else {                       /* toggle play/pause */
                        pl_paused = !pl_paused;
                        if (g_smpReady) { if (pl_paused) SMP_Pause(g_smp); else SMP_Play(g_smp); }
                    }
                    pl_hud_until = lastInput + 4500;
                }
            }
            else if (e.type == SDL_MOUSEBUTTONUP) {
                /* release a scrubber drag → commit the seek */
                lastInput = SDL_GetTicks();
                if (ptrDrag) {
                    ptrDrag = 0;
                    if (pl_scrub_ns >= 0) { g_seek_to_ns = pl_scrub_ns; pl_scrub_ns = -1; }
                    pl_hud_until = lastInput + 4500;
                }
            }
            else if (e.type == SDL_MOUSEWHEEL) {
                /* wheel = row up/down, Apple TV style (debounced: the
                 * Magic Remote wheel fires bursts of events per notch) */
                static Uint32 lastWheel = 0;
                Uint32 wnow = SDL_GetTicks();
                lastInput = wnow;
                if (wnow - lastWheel > 250) {
                    lastWheel = wnow;
                    if (e.wheel.y < 0 && fr < ROWS - 1) fr++;
                    else if (e.wheel.y > 0 && fr > 0) fr--;
                }
            }
        }
        Uint32 now = SDL_GetTicks();
        /* playback is user-driven now: OK on a shelf card calls start_bufferfeed().
         * DEV: /tmp/poc-autoplay auto-presses OK once, for headless screen tests. */
        static int autoTried = 0;
        if (!autoTried && !playing && now - t0 > 2000) {
            autoTried = 1;
            FILE *af = fopen("/tmp/poc-autoplay", "r");
            if (af) { fclose(af); playing = start_bufferfeed();
                      pl_paused = 0; pl_hud_until = now + 60000; }  /* dev: keep HUD up for capture */
        }
        /* dev: /tmp/poc-autoseek → one auto-seek to 40% at t0+12s (headless test) */
        static int seekTried = 0;
        if (!seekTried && playing && pl_dur_ns > 0 && now - t0 > 12000) {
            FILE *sf = fopen("/tmp/poc-autoseek", "r");
            if (sf) { fclose(sf); g_seek_to_ns = 140LL * 1000000000LL; }  /* dev: seek to 2:20 */
            seekTried = 1;
        }
        if (bf_started) bufferfeed_pump(now);
        /* client-side long-press repeat for the shelf: 400ms delay, then every 130ms */
        if (heldSym && now - heldSince > 400 && now - lastRep > 130) {
            lastRep = now;
            MOVE_FOCUS(heldSym);
        }
        /* LEFT/RIGHT scrub: advance the preview at a steady rate while the key is
         * held; commit on key-up (above). The remote's auto-repeat has a ~500ms
         * initial delay, so DON'T commit on a short idle gap — only a long safety
         * fallback (in case a key-up is ever missed). Pointer drag commits on up. */
        if (pl_scrub_ns >= 0 && scrubDir != 0 && !ptrDrag) {
            if (now - scrubLast > 1200) {           /* lost the key-up → commit */
                g_seek_to_ns = pl_scrub_ns; pl_scrub_ns = -1; scrubDir = 0; scrubT = 0;
            } else {                                /* held → ~35s of film per sec */
                float sdt = scrubT ? (now - scrubT) / 1000.0f : 0.016f;
                if (sdt > 0.1f) sdt = 0.1f;
                pl_scrub_ns += (long long)((double)scrubDir * 35.0 * sdt * 1e9);
                long long cap = pl_dur_ns - 3LL * 1000000000LL;
                if (pl_scrub_ns < 0) pl_scrub_ns = 0;
                if (cap > 0 && pl_scrub_ns > cap) pl_scrub_ns = cap;
                pl_hud_until = now + 4500; scrubT = now;
            }
        }
        /* hide the Magic Remote pointer after it's been idle during playback */
        if (playing && !curHidden && !ptrDrag && lastPtrMotion && now - lastPtrMotion > 3000) {
            SDL_webOSCursorVisibility(0);
            curHidden = 1;
        }
        /* re-pause after a resume: keep feeding until the resume seek is consumed and
         * its frame is on screen (a few frames presented), then pause where the user left off */
        if (resumePausePending && playing && !pl_paused && g_seek_to_ns < 0 && bf_frames >= 3 &&
            g_playpos_ns + 15LL * 1000000000LL >= bgPos) {   /* near the resume point, not the play-from-start */
            pl_paused = 1; if (g_smpReady) SMP_Pause(g_smp);
            resumePausePending = 0;
        }
        /* auto-demo only when launched with demo param */
        if (demo && now - lastInput > 6000 && now - lastAuto > 900) {
            lastAuto = now;
            fc += autodir;
            if (fc >= COLS) { fc = COLS - 1; autodir = -1; fr = (fr + 1) % ROWS; }
            else if (fc < 0) { fc = 0; autodir = 1; fr = (fr + 1) % ROWS; }
        }
        static Uint32 prev = 0;
        float dt = prev ? (now - prev) / 1000.0f : 0.016f;
        if (dt > 0.05f) dt = 0.05f;
        prev = now;
        bgPhase += dt * 0.15f;

        /* springs */
        for (int r = 0; r < ROWS; r++)
            for (int c = 0; c < COLS; c++)
                spring(&scale[r][c], &scaleV[r][c],
                       (r == fr && c == fc) ? 1.09f : 1.0f, 320.0f, dt);
        float targetSX = fc * (CARD_W + GAP) - 0.0f;
        if (targetSX < 0) targetSX = 0;
        float maxSX = COLS * (CARD_W + GAP) - GAP - (SCR_W - 2 * MARGIN_X);
        if (targetSX > maxSX) targetSX = maxSX;
        /* keep focused card near left third */
        float want = fc * (CARD_W + GAP) - (CARD_W + GAP);
        if (want < 0) want = 0;
        if (want > maxSX) want = maxSX;
        spring(&scrollX[fr], &scrollXV[fr], want, 170.0f, dt);
        float wantY = fr * ROW_PITCH - ROW_PITCH * 0.6f;
        if (wantY < 0) wantY = 0;
        float maxY = ROWS * ROW_PITCH - (SCR_H - CONTENT_Y) + 60.0f;
        if (maxY < 0) maxY = 0;
        if (wantY > maxY) wantY = maxY;
        spring(&scrollY, &scrollYV, wantY, 170.0f, dt);

        /* ---- draw ---- */
        glViewport(0, 0, SCR_W, SCR_H);
        if (playing) {
            /* Player: keep the graphics plane transparent so the video plane
             * shows through; overlay the transport HUD on interaction. */
            clear_opaque_region();
            glClearColor(0.0f, 0.0f, 0.0f, 0.0f);
            glClear(GL_COLOR_BUFFER_BIT);
            if (now < pl_hud_until || pl_paused) draw_hud();
            SDL_GL_SwapWindow(win);
            frames++;
            if (now - fpsT >= 1000) { frames = 0; fpsT = now; }
            continue;
        }
        /* ambient accent: cheap, animated clear color instead of a
         * fullscreen quad through the SDF shader (fill-rate!) */
        glClearColor(0.07f + 0.015f * sinf(bgPhase), 0.075f, 0.10f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);

        /* header: title block + tab pills (skeleton style) */
        float white[4] = {0.92f, 0.92f, 0.95f, 1.0f};
        float dimmer[4] = {0.25f, 0.26f, 0.30f, 1.0f};
        float dim2[4] = {0.18f, 0.19f, 0.22f, 1.0f};
        draw_rect(MARGIN_X, 64, 360, 56, 0, 14, white, white, 0);
        for (int i = 0; i < 4; i++)
            draw_rect(MARGIN_X + 480 + i * 180, 70, 150, 44, 0, 22,
                      dimmer, dim2, 0);

        /* shelves */
        for (int r = 0; r < ROWS; r++) {
            float rowY = CONTENT_Y + r * ROW_PITCH - scrollY;
            if (rowY > SCR_H || rowY + ROW_PITCH < 0) continue;
            /* row title bar */
            draw_rect(MARGIN_X, rowY, 240, 24, 0, 8, dimmer, dimmer, 0);
            for (int c = 0; c < COLS; c++) {
                if (r == fr && c == fc) continue; /* focused drawn last */
                float x = MARGIN_X + c * (CARD_W + GAP) - scrollX[r];
                if (x > SCR_W || x + CARD_W < -GLOW_PAD) continue;
                float y = rowY + ROW_TITLE_H + 18;
                float s = scale[r][c];
                float w = CARD_W * s, h = CARD_H * s;
                float cx = x - (w - CARD_W) / 2, cy = y - (h - CARD_H) / 2;
                float foc = (r == fr && c == fc) ? 1.0f : 0.0f;
                /* glow padding (and its fill cost) only on the focused card */
                float padpx = foc > 0.0f ? GLOW_PAD : 4.0f;
                draw_rect(cx - padpx, cy - padpx,
                          w + 2 * padpx, h + 2 * padpx,
                          padpx, 18.0f * s,
                          colTop[r][c], colBot[r][c],
                          foc * ((scale[r][c] - 1.0f) / 0.09f));
            }
        }

        /* focused card drawn last: glow overlays neighbors on BOTH sides */
        {
            float rowY = CONTENT_Y + fr * ROW_PITCH - scrollY;
            float x = MARGIN_X + fc * (CARD_W + GAP) - scrollX[fr];
            float y = rowY + ROW_TITLE_H + 18;
            float s = scale[fr][fc];
            float w = CARD_W * s, h = CARD_H * s;
            float cx = x - (w - CARD_W) / 2, cy = y - (h - CARD_H) / 2;
            draw_rect(cx - GLOW_PAD, cy - GLOW_PAD,
                      w + 2 * GLOW_PAD, h + 2 * GLOW_PAD,
                      GLOW_PAD, 18.0f * s, colTop[fr][fc], colBot[fr][fc],
                      (scale[fr][fc] - 1.0f) / 0.09f);
        }

        /* FPS counter */
        float fpsCol[4] = {0.4f, 1.0f, 0.55f, 1.0f};
        draw_number(fpsShown, SCR_W - 70, 64, 46, fpsCol);

        SDL_GL_SwapWindow(win);
        frames++;
        if (now - fpsT >= 1000) {
            fpsShown = (int)(frames * 1000.0f / (now - fpsT) + 0.5f);
            printf("FPS %d\n", fpsShown);
            fflush(stdout);
            frames = 0;
            fpsT = now;
        }
    }
    if (bf_started) stop_bufferfeed(0);
    SDL_Quit();
    return 0;
}
