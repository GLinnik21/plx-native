/* starfish.c — see starfish.h. The StarfishMediaAPIs (C++) + ACB ABI seam.
 * Every C++-ABI hazard lives here: the mangled __asm__ symbols, Feed's sret
 * std::string, the over-allocated in-place object (never new/delete), and the
 * 3-arg ACB taskId out-param. Callers touch only the flat sf_ / acb_ verbs. */
#include "starfish.h"
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>

extern FILE *elogf;   /* the event log (owned by the boot shim) */

/* ---------------------------------------------------------------------------
 * The video-plane binding, in its two mutually-exclusive forms.
 *
 * Decoded video does not reach the panel by itself: something has to tell the TV to punch a hole
 * in the compositor and route the pipeline's sink into it. LG changed how, exactly once, at
 * webOS 5.0 — and the two mechanisms are complementary across every firmware, never both:
 *
 *   webOS 2.2.3 .. 4.10.0   libAcbAPI.so.1 present, SDL exports no exported-window entry points
 *   webOS 5.3.1 .. 11.2.0   libAcbAPI.so.1 DELETED, SDL exports all five of them
 *
 * (Verified against 14 real firmware inventories — `tools/fwcompat.py --inventory libAcbAPI`.)
 *
 * So this file resolves BOTH at runtime and picks. libAcbAPI is dlopen'd rather than linked
 * because a DT_NEEDED entry for a library that does not exist kills the process at exec(), before
 * main, before the event log is open — which is what a webOS 5 owner sees today: nothing happens,
 * and there is nothing to send back. The SDL five are dlsym'd out of the already-loaded libSDL2
 * for the same reason in reverse: naming them at link time would make the binary demand symbols
 * that webOS 4.5 does not have.
 *
 * NOTHING BELOW THE RESOLUTION IS DEVICE-VERIFIED ON WEBOS 5. The symbols are proven present and
 * the call shapes come from the two implementations that ship (Kodi and mariotaku's ss4s); that a
 * picture actually lands on the plane is not something a symbol table can tell you, and the author
 * has no webOS 5 television. Treat `VP_EXPORTED` as untested code that is known to compile and
 * known to be calling the right names.
 * ------------------------------------------------------------------------- */

/* Same layout as SDL_Rect, which has been four ints since SDL2 existed. Declared here rather than
 * pulling SDL's headers into this file — the repo pins SDL 2.0.4 headers that predate SDL_webOS.h,
 * so an #include would resolve to a header without the prototypes we need anyway. */
typedef struct { int x, y, w, h; } VpRect;

static struct {
    long (*create)(void);
    int  (*initialize)(long, int, const char *, void (*)(long, long, long, long, long, const char *));
    int  (*setSinkType)(long, int);
    int  (*setMediaId)(long, const char *);
    /* 3-arg ABI CONFIRMED: createTask(TaskType, long*) writes the task id through arg3 — the
     * 2-arg form leaves garbage in r2 and segfaults / corrupts memory. Audio is owned by the
     * pipeline: we never hand ACB an audio SINK or elementary stream, and that half of the old
     * rule stands. What does NOT stand is its stated consequence — `SOUND_ERROR_019` appears in
     * no library on this device, and the clause carried no evidence from the initial commit
     * onward. `setMediaAudioData` is used, for a two-key METADATA descriptor: see
     * acb_send_atmos(). */
    int  (*setMediaVideoData)(long, const char *, long *);
    int  (*setState)(long, int, int, long *);
    int  (*setDisplayWindow)(long, long, long, long, long, int, long *);
    /* OPTIONAL — deliberately absent from the all-present gate below, and this is the one
     * pointer here that may legitimately be NULL. `AcbAPI_setMediaAudioData` is exported on
     * webOS 3.9.2 / 4.4.2 / 4.10.0 but NOT on 2.2.3 or 3.4.0 (tools/fwcompat.py --lib
     * libAcbAPI.so.1.0.0 --grep setMedia). Adding it to the AND would refuse ACB outright on
     * those two releases and take ALL VIDEO with it — a Dolby Atmos read-out is not worth a
     * black screen. See acb_send_atmos(). */
    int  (*setMediaAudioData)(long, const char *, long *);
} acb;

static struct {
    const char *(*createExportedWindow)(int type);
    int  (*setExportedWindow)(const char *windowId, VpRect *src, VpRect *dst);
    void (*destroyExportedWindow)(const char *windowId);
} sdlvp;

static int g_vp_mode = -1;                  /* -1 = not yet resolved; else a VP_* value */
static char g_window_id[64];                /* our copy of SDL's string — see vp_create_window */

/* The webOS 5 exported-window type. The real header spells the constant EXPORED, not EXPORTED
 * (LG's typo, present verbatim in the NDK's SDL_webOS.h); the literal avoids inheriting it. */
#define VP_EXPORTED_TYPE_VIDEO 0

int vp_mode(void) {
    if (g_vp_mode >= 0) return g_vp_mode;

    /* ACB first: on a 4.x set it is the only path, and probing SDL there costs five failed
     * lookups. RTLD_NOW so a half-usable library is rejected here rather than mid-playback. */
    void *h = dlopen("libAcbAPI.so.1", RTLD_NOW | RTLD_GLOBAL);
    if (h) {
        acb.create            = dlsym(h, "AcbAPI_create");
        acb.initialize        = dlsym(h, "AcbAPI_initialize");
        acb.setSinkType       = dlsym(h, "AcbAPI_setSinkType");
        acb.setMediaId        = dlsym(h, "AcbAPI_setMediaId");
        acb.setMediaVideoData = dlsym(h, "AcbAPI_setMediaVideoData");
        acb.setState          = dlsym(h, "AcbAPI_setState");
        acb.setDisplayWindow  = dlsym(h, "AcbAPI_setDisplayWindow");
        /* Optional; NULL on 2.2.3/3.4.0 and acb_send_atmos() simply does nothing there. */
        acb.setMediaAudioData = dlsym(h, "AcbAPI_setMediaAudioData");
        /* Every pointer assigned above, checked. This list used to name seven of the eight and
         * drop `destroy` silently — which is the argument against hand-enumeration, and why
         * `destroy` and the two unused SDL crop/property pointers are gone rather than resolved
         * and forgotten. Add one here only when something calls it. */
        if (acb.create && acb.initialize && acb.setSinkType && acb.setMediaId &&
            acb.setMediaVideoData && acb.setState && acb.setDisplayWindow) {
            g_vp_mode = VP_ACB;
            if (elogf) { fprintf(elogf, "vplane: ACB (webOS 4.x)\n"); fflush(elogf); }
            return g_vp_mode;
        }
        if (elogf) { fprintf(elogf, "vplane: libAcbAPI.so.1 opened but is missing entry points\n"); fflush(elogf); }
    }

    /* RTLD_DEFAULT: libSDL2 is a normal DT_NEEDED dependency, so its symbols are already in the
     * global scope. This asks "did the SDL that actually loaded bring these", which is exactly
     * the question — the NDK's sysroot copy carries stub bodies for them, so a link-time check
     * would have said yes on every device and meant nothing. */
    sdlvp.createExportedWindow  = dlsym(RTLD_DEFAULT, "SDL_webOSCreateExportedWindow");
    sdlvp.setExportedWindow     = dlsym(RTLD_DEFAULT, "SDL_webOSSetExportedWindow");
    sdlvp.destroyExportedWindow = dlsym(RTLD_DEFAULT, "SDL_webOSDestroyExportedWindow");
    if (sdlvp.createExportedWindow && sdlvp.setExportedWindow) {
        g_vp_mode = VP_EXPORTED;
        if (elogf) { fprintf(elogf, "vplane: SDL exported window (webOS 5+)\n"); fflush(elogf); }
        return g_vp_mode;
    }

    g_vp_mode = VP_NONE;
    if (elogf) { fprintf(elogf, "vplane: NONE — no libAcbAPI and no SDL exported window; video cannot be shown\n"); fflush(elogf); }
    return g_vp_mode;
}

/* Create the exported window and return its id, or NULL. VP_EXPORTED only.
 *
 * Must be called BEFORE Load, because the id has to travel inside the Load payload as
 * option.windowId — that string is the whole of the binding on webOS 5. The compositor assigns it
 * ("_Window_Id_<n>") and SDL hands back a pointer whose ownership and lifetime LG does not
 * document, so it is copied immediately; ss4s does the same. */
const char *vp_create_window(void) {
    if (vp_mode() != VP_EXPORTED || !sdlvp.createExportedWindow) return 0;
    const char *id = sdlvp.createExportedWindow(VP_EXPORTED_TYPE_VIDEO);
    if (!id) {
        if (elogf) { fprintf(elogf, "vplane: SDL_webOSCreateExportedWindow returned NULL\n"); fflush(elogf); }
        return 0;
    }
    snprintf(g_window_id, sizeof g_window_id, "%s", id);
    if (elogf) { fprintf(elogf, "vplane: exported windowId=%s\n", g_window_id); fflush(elogf); }
    return g_window_id;
}

/* The compositor-assigned exported windowId, or "" when there is none — for the on-screen
 * diagnostics read-out (`ui::stats`), which needs to say whether the window was ever created.
 * Returns this module's own copy (see vp_create_window), so the pointer is permanently valid and
 * the caller never owns it. Never NULL: an empty string IS the "no window" answer. */
const char *vp_window_id(void) { return g_window_id; }

/* Place the video: source frame size -> on-screen rect. The VP_EXPORTED counterpart of
 * acb_start's setDisplayWindow, and the pair also expresses scaling. */
int vp_place(int src_w, int src_h, int dst_x, int dst_y, int dst_w, int dst_h) {
    if (vp_mode() != VP_EXPORTED || !g_window_id[0] || !sdlvp.setExportedWindow) return 0;
    VpRect src = { 0, 0, src_w, src_h };
    VpRect dst = { dst_x, dst_y, dst_w, dst_h };
    int rv = sdlvp.setExportedWindow(g_window_id, &src, &dst);
    if (elogf) {
        fprintf(elogf, "vplane: place %dx%d -> %d,%d %dx%d rv=%d\n",
                src_w, src_h, dst_x, dst_y, dst_w, dst_h, rv);
        fflush(elogf);
    }
    return rv;
}

void vp_destroy_window(void) {
    if (g_window_id[0] && sdlvp.destroyExportedWindow) sdlvp.destroyExportedWindow(g_window_id);
    g_window_id[0] = 0;
}

#define SINK_TYPE_MAIN      0
#define APPSTATE_FOREGROUND 1
#define PLAYSTATE_UNLOADED  0
#define PLAYSTATE_LOADED    1
#define PLAYSTATE_PLAYING   2
#define PLAYSTATE_PAUSED    3

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
extern int  SMP_flush(void *self) __asm__("_ZN17StarfishMediaAPIs5flushEv");
/* Kodi-parity: signal true end-of-stream so the pipeline drains the last frames instead of
 * hanging on them. Verified present on webOS 4.5 (nm: _ZN17StarfishMediaAPIs7pushEOSEv, defined). */
extern int  SMP_pushEOS(void *self) __asm__("_ZN17StarfishMediaAPIs7pushEOSEv");
/* Kodi in-place seek: setTimeToDecode(JSON {"position":<ns>}) + the CustomPipeline's
 * sendSegmentEvent() (called ON the pipeline pointer, reached from the object below). */
extern int  SMP_setTimeToDecode(void *self, const char *json)
    __asm__("_ZN17StarfishMediaAPIs15setTimeToDecodeEPKc");
extern void CP_sendSegmentEvent(void *pipeline)
    __asm__("_ZN13mediapipeline14CustomPipeline16sendSegmentEventEv");
/* webOS<11 in-place-seek fallback (the path the official app / Kodi use): set the pipeline's
 * decode PTS through its MEDIA_CUSTOM_CONTENT_INFO. loadSpi_getInfo(ci) fills the whole 312-byte
 * struct from current state (it internally memsets 0x138 + repopulates via config_contentinfo);
 * we then overwrite only ptsToDecode (int64 @ +0x28) and setContentInfo() memcpy's it back into
 * the running pipeline. Both are CustomPipeline methods → `this` = the pipeline ptr. Verified by
 * decompile of libpf-1.0.so.1: struct size 0x138, ptsToDecode @ 0x28, SRC_TYPE_ES = 7. */
extern int  CP_loadSpi_getInfo(void *pipeline, void *ci)
    __asm__("_ZN13mediapipeline14CustomPipeline15loadSpi_getInfoEP25MEDIA_CUSTOM_CONTENT_INFO");
extern void CP_setContentInfo(void *pipeline, int srcType, void *ci)
    __asm__("_ZN13mediapipeline14CustomPipeline14setContentInfoE23MEDIA_CUSTOM_SRC_TYPE_TP25MEDIA_CUSTOM_CONTENT_INFO");
#define MEDIA_CUSTOM_SRC_TYPE_ES 7   /* MEDIA_CUSTOM_SRC_TYPE_T::ES (verified) */
#define CI_SIZE      0x138           /* sizeof(MEDIA_CUSTOM_CONTENT_INFO_T) = 312 (verified) */
#define CI_PTS_OFF   0x28            /* int64 ptsToDecode offset (verified 3 ways) */

static unsigned char g_smp[65536] __attribute__((aligned(16)));
/* Publishes the 64 KB in-place-constructed StarfishMediaAPIs object ACROSS THREADS: written
   on the load thread (player/threads.rs load_thread) right after SMP_ctor, read on the main
   thread every frame (pump.rs). A plain int gives the compiler and the A53 licence to make
   the store visible before the constructor's writes, so the main thread could dispatch
   SMP_Play / sf_feed through a half-built object — a startup-only, timing-dependent SIGSEGV
   inside libplayerAPIs. Release/acquire pairs the flag with the ctor that precedes it. */
static volatile int g_smp_ready = 0;
#define SMP_READY()      __atomic_load_n(&g_smp_ready, __ATOMIC_ACQUIRE)
#define SMP_SET_READY(v) __atomic_store_n(&g_smp_ready, (v), __ATOMIC_RELEASE)
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
    SMP_SET_READY(1);        /* RELEASE: the ctor's writes must be visible before the flag */
    SMP_notifyForeground(g_smp);
    return SMP_Load(g_smp, payload, sf_cb);
}
int  sf_ready(void)               { return SMP_READY(); }
int  sf_is_load_completed(void)   { return SMP_READY() ? SMP_isLoadCompleted(g_smp) : 0; }
int  sf_play(void)                { return SMP_READY() ? SMP_Play(g_smp) : 0; }
int  sf_pause(void)               { return SMP_READY() ? SMP_Pause(g_smp) : 0; }
int  sf_flush(void)               { return SMP_READY() ? SMP_flush(g_smp) : 0; }
int  sf_push_eos(void)            { return SMP_READY() ? SMP_pushEOS(g_smp) : 0; }
void sf_unload(void)              { if (SMP_READY()) SMP_Unload(g_smp); }
void sf_destroy(void)             { if (SMP_READY()) { SMP_dtor(g_smp); SMP_SET_READY(0); } }

/* The CustomPipeline* reached from our object (VERIFIED by decompile: StarfishMediaAPIs::
 * player is a shared_ptr _M_ptr at g_smp+0x4c; AbstractPlayer::pipeline _M_ptr at player+0x4;
 * Pipeline is CustomPipeline's primary base so the ptr is usable as `this`). player@0x4c is
 * populated on our uid=NULL object — sf_play/sf_flush already dispatch through it. */
static void *sf_pipeline(void) {
    if (!SMP_READY()) return 0;
    void *player = *(void **)((unsigned char *)g_smp + 0x4c);
    if (!player) return 0;
    return *(void **)((unsigned char *)player + 0x04);
}

/* Kodi in-place seek, on the first video AU after flush(): tell the pipeline the new decode
 * timestamp, then inject a fresh GStreamer SEGMENT (the step a bare flush() omits — its
 * absence is the stale-segment stall the reload path works around). position_ns = the fed
 * (0-based rebased) PTS of that first frame. */
int sf_set_time_to_decode(long long position_ns) {
    if (!SMP_READY()) return 0;
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

/* webOS<11 in-place seek: the fallback for when setTimeToDecode returns 0 (which it does on
 * this build — it dispatches to CustomPlayer::setTimeToDecodeSpi, which only succeeds while the
 * pipeline is in PausedState). Over-allocated struct (real size 0x138; the app never sees the
 * layout — libpf owns it). Call on the first post-flush keyframe, BEFORE sf_send_segment().
 * Returns 0 only if the pipeline ptr isn't reachable. */
static unsigned char g_ci[1024] __attribute__((aligned(16)));
int sf_set_content_info(long long position_ns) {
    void *p = sf_pipeline();
    if (!p) return 0;
    memset(g_ci, 0, sizeof g_ci);
    CP_loadSpi_getInfo(p, g_ci);                       /* fill current content info */
    *(long long *)(g_ci + CI_PTS_OFF) = position_ns;   /* override ptsToDecode (ns) */
    CP_setContentInfo(p, MEDIA_CUSTOM_SRC_TYPE_ES, g_ci);
    if (elogf) { fprintf(elogf, "setContentInfo: pts=%lld ES=%d\n",
                         position_ns, MEDIA_CUSTOM_SRC_TYPE_ES); fflush(elogf); }
    return 1;
}

/* Feed one AU; hides the sret std::string (SSO char* at offset 0). 'O'/'B'/'e'. */
char sf_feed(const unsigned char *p, unsigned size, long long pts, int esData) {
    /* The only verb that used to dispatch with NO readiness check — it relied entirely on
       pump.rs returning early. Guard it here too so the object can never be fed mid-ctor. */
    if (!SMP_READY()) return 'e';
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

/* ---- ACB verbs (the 3-arg taskId ABI is hidden) ----
 *
 * Every one is a no-op when this device has no ACB, so the caller does not have to branch: on
 * webOS 5 the whole setSinkType / setMediaId / setMediaVideoData / setState sequence has no
 * replacement — it is simply deleted, which is what both reference implementations do (ss4s stubs
 * all of them to `return true`, Kodi guards each with `if (acb)`). */
long acb_create(const char *appId, int playerType) {
    if (vp_mode() != VP_ACB) return 0;
    g_acb = acb.create();
    if (g_acb) acb.initialize(g_acb, playerType, appId ? appId : "com.beb.plxnative", acb_cb);
    return g_acb;
}
void acb_bind(const char *mediaId) {
    if (!g_acb) return;
    acb.setSinkType(g_acb, SINK_TYPE_MAIN);
    acb.setMediaId(g_acb, mediaId);
    acb.setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_LOADED, &g_taskId);
}
/* Tell ACB the stream is Dolby Atmos — the AUDIO counterpart of acb_send_video_data(), and a
 * METADATA descriptor rather than anything resembling an audio sink.
 *
 * Recovered whole from this television's own binaries (2026-08-21). `libcbe.so`
 * `media::MediaAPIsWrapper::SetDolbyAtmosInfoToACB` @0x01b976f0 — the ONLY caller of the ACB
 * audio entry point anywhere in ~70 harvested libraries, and the path LG's own web apps take —
 * builds exactly this two-key object with jsoncpp and hands it to `Acb::setMediaAudioData` with
 * a NULL taskId. `AcbAPI_setMediaAudioData` @0x1836c is instruction-for-instruction the same
 * 316-byte shape as `AcbAPI_setMediaVideoData` @0x16fac, so the 3-arg taskId ABI applies here
 * unchanged; below it `ACB::AcbCore::setMediaAudioData` @0xfda4 parses the JSON, dedups against
 * its cached copy, and posts luna://com.webos.service.acb/setAudioInfo with
 * {"appId":…,"pipelineId":<mediaId>,"audioInfo":<this object>}. That is the whole path: a
 * validity check, a dedup and one async Luna post. It touches no sink, no elementary stream, no
 * codec descriptor and no second bind.
 *
 * `context` is the SAME string acb_bind() passed to setMediaId, and carrying it is the entire
 * reason LG synthesises this object instead of forwarding the pipeline's own AUDIO_INFO callback:
 * `StarfishMediaAPIs::handleAudioInfoEvent` builds that callback from `track` and `dualMono` only
 * and has no `context`, while `generateVideoInfoPtree` @0x3755c puts one in the VIDEO envelope we
 * already forward verbatim and which already works. Same object, one codec over.
 *
 * ON THE RULE THIS APPEARS TO BREAK. `src/starfish.c` and `player/CLAUDE.md` have said since the
 * initial commit that audio must never be fed to ACB because it causes `SOUND_ERROR_019`. That
 * literal exists in NO library on this device — swept three ways across the whole harvest,
 * including 92 MB of Chromium — and no log line containing it has ever been committed. It sits in
 * `f2523483` beside the 3-arg taskId note, which does carry its evidence. What the rule is
 * plainly RIGHT about is the audio ES: the pipeline owns it and we never hand ACB a sink. This is
 * not that, and the distinction is now readable in the disassembly rather than remembered.
 * The residual is the ACB DAEMON, whose binary is not a `.so` and so was never harvested — the
 * far side of that Luna post is unread. Hence the trigger: this is armed, not assumed.
 *
 * Returns 1 accepted, -1 our JSON was rejected client-side (nothing left the process), -2 ACB not
 * initialized, 0 no ACB / no symbol / no id. Call after acb_bind(), which is where libcbe fires
 * it (0x1b98d78, on LOADCOMPLETED) — no decoded frame is needed and no state is read. */
int acb_send_atmos(const char *mediaId) {
    if (!g_acb || !acb.setMediaAudioData || !mediaId) return 0;
    char j[192];
    /* The trailing newline is jsoncpp FastWriter's and is byte-for-byte what Chromium sends.
     * json-c ignores it; it is here for exactness, not because anything requires it. */
    snprintf(j, sizeof j, "{\"audio\":{\"immersive\":\"ATMOS\"},\"context\":\"%s\"}\n", mediaId);
    int rv = acb.setMediaAudioData(g_acb, j, &g_taskId);
    if (elogf) { fprintf(elogf, "acb setMediaAudioData rv=%d payload=%s", rv, j); fflush(elogf); }
    return rv;
}
int acb_send_video_data(const char *sourceInfoVerbatim) {
    if (!g_acb) return 0;
    return acb.setMediaVideoData(g_acb, sourceInfoVerbatim, &g_taskId);
}
void acb_start(long x, long y, long w, long h) {
    if (!g_acb) return;
    acb.setDisplayWindow(g_acb, x, y, w, h, 1, &g_taskId);
    acb.setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_PLAYING, &g_taskId);
}
void acb_unload(void) {
    if (g_acb) acb.setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_UNLOADED, &g_taskId);
}
/* Kodi-parity: mirror the ACB PLAYSTATE on transport pause/resume (the app owns the sink; the
 * pipeline Pause/Play alone leaves the ACB state stale). Only meaningful once the plane is bound. */
void acb_pause(void)  { if (g_acb) acb.setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_PAUSED,  &g_taskId); }
void acb_resume(void) { if (g_acb) acb.setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_PLAYING, &g_taskId); }
