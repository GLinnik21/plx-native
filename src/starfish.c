/* starfish.c — see starfish.h. The StarfishMediaAPIs (C++) + ACB ABI seam.
 * Every C++-ABI hazard lives here: the mangled __asm__ symbols, Feed's sret
 * std::string, the over-allocated in-place object (never new/delete), and the
 * 3-arg ACB taskId out-param. Callers touch only the flat sf_ / acb_ verbs. */
#include "starfish.h"
#include <stdio.h>
#include <string.h>

extern FILE *elogf;   /* the event log (owned by the boot shim) */

/* ---- ACB (App Common Binding) extern decls ---- */
extern long AcbAPI_create(void);
extern int  AcbAPI_initialize(long acbId, int playerType, const char *appId,
                              void (*cb)(long, long, long, long, long, const char *));
extern int  AcbAPI_setSinkType(long acbId, int sinkType);
extern int  AcbAPI_setMediaId(long acbId, const char *connId);
/* 3-arg ABI CONFIRMED: createTask(TaskType, long*) writes the task id through arg3
 * — the 2-arg form leaves garbage in r2 and segfaults / corrupts memory. Audio is
 * owned by the pipeline (never feed it to ACB → SOUND_ERROR_019), so
 * AcbAPI_setMediaAudioData is intentionally unused. */
extern int  AcbAPI_setMediaVideoData(long acbId, const char *payload, long *taskId);
extern int  AcbAPI_setState(long acbId, int appState, int playState, long *taskId);
extern int  AcbAPI_setDisplayWindow(long acbId, long x, long y, long w, long h,
                                    int fullScreen, long *taskId);
extern int  AcbAPI_finalize(long acbId);
extern void AcbAPI_destroy(long acbId);
#define SINK_TYPE_MAIN      0
#define APPSTATE_FOREGROUND 1
#define PLAYSTATE_UNLOADED  0
#define PLAYSTATE_LOADED    1
#define PLAYSTATE_PLAYING   2

/* ---- LG StarfishMediaAPIs (libplayerAPIs): mangled C++ symbols. `this` is an
 * over-allocated buffer we construct in place (object size unknown, so never
 * new/delete). Methods returning std::string use a hidden sret pointer (first
 * arg); we read the char* at offset 0 (SSO holds "Ok"/"BufferFull"). ---- */
extern void SMP_ctor(void *self, const char *appId) __asm__("_ZN17StarfishMediaAPIsC1EPKc");
extern void SMP_dtor(void *self) __asm__("_ZN17StarfishMediaAPIsD1Ev");
extern int  SMP_Load(void *self, const char *payload, void (*cb)(int, long long, const char *))
    __asm__("_ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_E");
extern void SMP_Feed(void *sret, void *self, const char *payload)
    __asm__("_ZN17StarfishMediaAPIs4FeedB5cxx11EPKc");
extern int  SMP_Play(void *self) __asm__("_ZN17StarfishMediaAPIs4PlayEv");
extern int  SMP_Unload(void *self) __asm__("_ZN17StarfishMediaAPIs6UnloadEv");
extern void SMP_notifyForeground(void *self) __asm__("_ZN17StarfishMediaAPIs16notifyForegroundEv");
extern int  SMP_isLoadCompleted(void *self) __asm__("_ZN17StarfishMediaAPIs15isLoadCompletedEv");
extern int  SMP_Pause(void *self) __asm__("_ZN17StarfishMediaAPIs5PauseEv");
extern void SMP_setCurrentPlaytime(void *self, long long t) __asm__("_ZN17StarfishMediaAPIs18setCurrentPlaytimeEx");
extern int  SMP_flush(void *self) __asm__("_ZN17StarfishMediaAPIs5flushEv");
/* Kodi in-place seek: setTimeToDecode(JSON {"position":<ns>}) + the CustomPipeline's
 * sendSegmentEvent() (called ON the pipeline pointer, reached from the object below). */
extern int  SMP_setTimeToDecode(void *self, const char *json)
    __asm__("_ZN17StarfishMediaAPIs15setTimeToDecodeEPKc");
extern void CP_sendSegmentEvent(void *pipeline)
    __asm__("_ZN13mediapipeline14CustomPipeline16sendSegmentEventEv");

static unsigned char g_smp[65536] __attribute__((aligned(16)));
static int  g_smp_ready = 0;
static long g_acb = 0, g_taskId = 0;

/* trampolines: forward library-thread events to the consumer's handlers */
static void sf_cb(int type, long long num, const char *str) { sf_on_event(type, num, str); }
static void acb_cb(long a, long t, long ev, long app, long play, const char *reply) {
    (void)a; (void)t; (void)app; (void)play;
    acb_on_event(ev, reply);
}

/* ---- StarfishMediaAPIs verbs ---- */
int sf_load(const char *payload) {
    SMP_ctor(g_smp, NULL);   /* uid=NULL: registers on the pre-authorized uMS namespace */
    g_smp_ready = 1;
    SMP_notifyForeground(g_smp);
    return SMP_Load(g_smp, payload, sf_cb);
}
int  sf_ready(void)               { return g_smp_ready; }
int  sf_is_load_completed(void)   { return g_smp_ready ? SMP_isLoadCompleted(g_smp) : 0; }
int  sf_play(void)                { return g_smp_ready ? SMP_Play(g_smp) : 0; }
int  sf_pause(void)               { return g_smp_ready ? SMP_Pause(g_smp) : 0; }
int  sf_flush(void)               { return g_smp_ready ? SMP_flush(g_smp) : 0; }
void sf_set_playtime(long long t) { if (g_smp_ready) SMP_setCurrentPlaytime(g_smp, t); }
void sf_unload(void)              { if (g_smp_ready) SMP_Unload(g_smp); }
void sf_destroy(void)             { if (g_smp_ready) { SMP_dtor(g_smp); g_smp_ready = 0; } }

/* The CustomPipeline* reached from our object (VERIFIED by decompile: StarfishMediaAPIs::
 * player is a shared_ptr _M_ptr at g_smp+0x4c; AbstractPlayer::pipeline _M_ptr at player+0x4;
 * Pipeline is CustomPipeline's primary base so the ptr is usable as `this`). player@0x4c is
 * populated on our uid=NULL object — sf_play/sf_flush already dispatch through it. */
static void *sf_pipeline(void) {
    if (!g_smp_ready) return 0;
    void *player = *(void **)((unsigned char *)g_smp + 0x4c);
    if (!player) return 0;
    return *(void **)((unsigned char *)player + 0x04);
}

/* Kodi in-place seek, on the first video AU after flush(): tell the pipeline the new decode
 * timestamp, then inject a fresh GStreamer SEGMENT (the step a bare flush() omits — its
 * absence is the stale-segment stall the reload path works around). position_ns = the fed
 * (0-based rebased) PTS of that first frame. */
int sf_set_time_to_decode(long long position_ns) {
    if (!g_smp_ready) return 0;
    char j[64];
    snprintf(j, sizeof j, "{\"position\":%lld}", position_ns);
    return SMP_setTimeToDecode(g_smp, j);
}
int sf_send_segment(void) {
    void *p = sf_pipeline();
    if (elogf) { fprintf(elogf, "sendSegment: pipeline=%p\n", p); fflush(elogf); }
    if (!p) return 0;
    CP_sendSegmentEvent(p);
    return 1;
}

/* Feed one AU; hides the sret std::string (SSO char* at offset 0). 'O'/'B'/'e'. */
char sf_feed(const unsigned char *p, unsigned size, long long pts, int esData) {
    char j[160];
    snprintf(j, sizeof j, "{\"bufferAddr\":\"%p\",\"bufferSize\":%u,\"pts\":%lld,\"esData\":%d}",
             (const void *)p, size, pts, esData);
    unsigned char ret[32];
    memset(ret, 0, sizeof ret);
    SMP_Feed(ret, g_smp, j);
    char *s = *(char **)ret;             /* std::string _M_p at offset 0 */
    static int logged = 0;
    if (elogf && logged < 3) { logged++;
        fprintf(elogf, "feed reply=\"%s\"\n", s ? s : "(null)"); fflush(elogf); }
    if (!s) return 'e';
    if (strstr(s, "BufferFull")) return 'B';
    if (strstr(s, "Ok")) return 'O';
    return 'e';
}

/* ---- ACB verbs (the 3-arg taskId ABI is hidden) ---- */
long acb_create(const char *appId, int playerType) {
    g_acb = AcbAPI_create();
    if (g_acb) AcbAPI_initialize(g_acb, playerType, appId ? appId : "com.glin.plexpoc", acb_cb);
    return g_acb;
}
void acb_bind(const char *mediaId) {
    AcbAPI_setSinkType(g_acb, SINK_TYPE_MAIN);
    AcbAPI_setMediaId(g_acb, mediaId);
    AcbAPI_setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_LOADED, &g_taskId);
}
int acb_send_video_data(const char *sourceInfoVerbatim) {
    return AcbAPI_setMediaVideoData(g_acb, sourceInfoVerbatim, &g_taskId);
}
void acb_start(long x, long y, long w, long h) {
    AcbAPI_setDisplayWindow(g_acb, x, y, w, h, 1, &g_taskId);
    AcbAPI_setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_PLAYING, &g_taskId);
}
void acb_unload(void) {
    if (g_acb) AcbAPI_setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_UNLOADED, &g_taskId);
}
void acb_destroy(void) {
    if (g_acb) { AcbAPI_finalize(g_acb); AcbAPI_destroy(g_acb); g_acb = 0; }
}
