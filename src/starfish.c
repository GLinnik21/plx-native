/* starfish.c — see starfish.h. The StarfishMediaAPIs (C++) + ACB ABI seam.
 * Every C++-ABI hazard lives here: the mangled __asm__ symbols, Feed's sret
 * std::string, the over-allocated in-place object (never new/delete), and the
 * 3-arg ACB taskId out-param. Callers touch only the flat sf_ / acb_ verbs. */
#include "starfish.h"
#include <dlfcn.h>
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
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
/* Callback-context overload, device-proven on webOS 4.10.2:
 *   _ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_PvES2_
 * stores cb at object+0x9c and ctx at +0xa0, then tail-calls the ordinary Load. The tail call
 * preserves Load's integer result in r0. `sendCallbackEvent` dispatches this callback with that
 * exact context. This is what lets Rust reject a late event from a retired Load without guessing
 * from PTS or payload contents. It is dlsym'd rather than a strong ELF reference because only this
 * firmware artifact proves the overload's behaviour: a release which lacks it must refuse playback
 * at runtime, not prevent the whole app from reaching main. The firmware inventories prove only
 * that the symbol exists across supported releases; they cannot prove the context dispatch
 * described by the device/decompile evidence above. */
typedef int (*SMP_LoadWithContextFn)(
    void *self,
    const char *payload,
    void (*cb)(int, long long, const char *, void *),
    void *ctx);
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

/* callbackFunctionHook's exact ABI on ARM32/AAPCS is:
 *
 *   r0 = StarfishMediaAPIs *this, r1 = int, r2/r3 = aligned int64, stack[0] = const char *
 *
 * libplayerAPIs' fixed libpf callback thunk passes the original Starfish object as `this`, then
 * reaches this symbol through its R_ARM_JUMP_SLOT. The executable exports this definition so it
 * preempts the library definition; RTLD_NEXT below resolves the real implementation. The gate
 * counts an admitted call as in-flight across the complete real hook without holding a mutex
 * during that call (a nested firmware callback must not deadlock). After active is cleared, new
 * calls drop before dereferencing the object and teardown waits without a timeout for in-flight
 * to reach zero.
 *
 * A slot, including the 64 KB object ADDRESS, is never freed or reused. A late libpf thunk has no
 * generation token, only this address, so reusing the old g_smp address would let it select a new
 * session before Rust ever saw the callback. Retired slots remain in the registry until process
 * exit; the interposer looks them up without dereferencing their destroyed object storage. */
#define SMP_STORAGE_SIZE 65536u
#define SMP_CALLBACK_HOOK_SYMBOL "_ZN17StarfishMediaAPIs20callbackFunctionHookEixPKc"
#define SMP_LOAD_WITH_CONTEXT_SYMBOL "_ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_PvES2_"

typedef void (*SMP_CallbackHook)(void *, int, long long, const char *);
_Static_assert(sizeof(void *) == 4, "Starfish seam requires the 32-bit ARM ABI");
_Static_assert(sizeof(unsigned int) == 4, "Rust epoch/c_uint width mismatch");
_Static_assert(sizeof(long long) == 8, "Rust callback i64 width mismatch");

typedef struct SfSlot {
    struct SfSlot *next;
    pthread_mutex_t gate;
    pthread_cond_t drained;
    unsigned int epoch;
    unsigned int intercepted;
    unsigned int dropped;
    unsigned int inflight;       /* protected by gate */
    int active;                 /* protected by gate */
    int evidence_armed;         /* protected by gate; true only once this Load is entering */
    int gate_retired;           /* protected by gate */
    volatile int unload_completed;
    volatile int destroyed;
    unsigned char object[SMP_STORAGE_SIZE] __attribute__((aligned(16)));
} SfSlot;
_Static_assert(_Alignof(SfSlot) >= 16, "Starfish slot allocation must preserve 16-byte alignment");
_Static_assert(offsetof(SfSlot, object) % 16 == 0, "Starfish object offset lost alignment");

static pthread_mutex_t g_slots_lock = PTHREAD_MUTEX_INITIALIZER;
static SfSlot *g_slots;
static SfSlot *g_current;
static pthread_once_t g_hook_once = PTHREAD_ONCE_INIT;
static SMP_CallbackHook g_real_callback_hook;
static SMP_LoadWithContextFn g_load_with_context;
static volatile int g_hook_valid;
static volatile int g_lifecycle_blocked;

void sf_callback_hook_interposer(void *self, int type, long long num, const char *str)
    __asm__(SMP_CALLBACK_HOOK_SYMBOL);

static void sf_resolve_callback_hook_once(void) {
    dlerror();
    SMP_CallbackHook real = (SMP_CallbackHook)dlsym(RTLD_NEXT, SMP_CALLBACK_HOOK_SYMBOL);
    int hook_lookup_ok = dlerror() == NULL;
    Dl_info owner, load_owner;
    memset(&owner, 0, sizeof owner);
    memset(&load_owner, 0, sizeof load_owner);
    int owner_ok = real && real != sf_callback_hook_interposer &&
                   dladdr((void *)real, &owner) != 0 && owner.dli_fname &&
                   strstr(owner.dli_fname, "libplayerAPIs.so") != NULL;

    dlerror();
    SMP_LoadWithContextFn load =
        (SMP_LoadWithContextFn)dlsym(RTLD_DEFAULT, SMP_LOAD_WITH_CONTEXT_SYMBOL);
    int load_lookup_ok = dlerror() == NULL;
    int load_owner_ok = load && dladdr((void *)load, &load_owner) != 0 &&
                        load_owner.dli_fname &&
                        strstr(load_owner.dli_fname, "libplayerAPIs.so") != NULL;
    if (hook_lookup_ok && owner_ok && load_lookup_ok && load_owner_ok) {
        g_real_callback_hook = real;
        g_load_with_context = load;
        __atomic_store_n(&g_hook_valid, 1, __ATOMIC_RELEASE);
        if (elogf) {
            fprintf(elogf,
                    "native callback gate: hook=%p owner=%s loadCtx=%p loadOwner=%s\n",
                    (void *)real, owner.dli_fname, (void *)load, load_owner.dli_fname);
            fflush(elogf);
        }
        return;
    }
    __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
    if (elogf) {
        fprintf(elogf,
                "native callback gate: BLOCKED symbol=%s real=%p owner=%s hookLookup=%d "
                "loadSymbol=%s load=%p loadOwner=%s loadLookup=%d\n",
                SMP_CALLBACK_HOOK_SYMBOL, (void *)real,
                owner.dli_fname ? owner.dli_fname : "(none)",
                hook_lookup_ok, SMP_LOAD_WITH_CONTEXT_SYMBOL, (void *)load,
                load_owner.dli_fname ? load_owner.dli_fname : "(none)",
                load_lookup_ok);
        fflush(elogf);
    }
}

static int sf_prepare_callback_hook(void) {
    if (pthread_once(&g_hook_once, sf_resolve_callback_hook_once) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        return 0;
    }
    return __atomic_load_n(&g_hook_valid, __ATOMIC_ACQUIRE);
}

/* `lookup_ok == 0` means the registry lock itself failed: fail closed instead of forwarding a
 * possibly-owned object without its gate. A successful lookup with no match is an object owned by
 * some other in-process client and must retain libplayerAPIs' ordinary behaviour. */
static SfSlot *sf_find_slot(void *object, int *lookup_ok) {
    *lookup_ok = 0;
    if (pthread_mutex_lock(&g_slots_lock) != 0) return NULL;
    SfSlot *slot = g_slots;
    while (slot && slot->object != (unsigned char *)object) slot = slot->next;
    *lookup_ok = pthread_mutex_unlock(&g_slots_lock) == 0;
    return *lookup_ok ? slot : NULL;
}

__attribute__((visibility("default"), externally_visible, used))
void sf_callback_hook_interposer(void *self, int type, long long num, const char *str) {
    if (!sf_prepare_callback_hook()) return;

    int lookup_ok = 0;
    SfSlot *slot = sf_find_slot(self, &lookup_ok);
    if (!lookup_ok) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        return;
    }
    if (!slot) {
        g_real_callback_hook(self, type, num, str);
        return;
    }

    if (pthread_mutex_lock(&slot->gate) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        return;
    }
    if (slot->active) {
        if (slot->evidence_armed)
            __atomic_add_fetch(&slot->intercepted, 1u, __ATOMIC_RELAXED);
        ++slot->inflight;
    } else {
        __atomic_add_fetch(&slot->dropped, 1u, __ATOMIC_RELAXED);
        if (pthread_mutex_unlock(&slot->gate) != 0)
            __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        return;
    }
    if (pthread_mutex_unlock(&slot->gate) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        return;
    }

    g_real_callback_hook(self, type, num, str);

    if (pthread_mutex_lock(&slot->gate) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        return;
    }
    if (slot->inflight == 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
    } else if (--slot->inflight == 0 && pthread_cond_broadcast(&slot->drained) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
    }
    if (pthread_mutex_unlock(&slot->gate) != 0)
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
}

/* Publishes the per-session in-place-constructed object ACROSS THREADS: written on the load
   thread right after SMP_ctor, read on the main thread every frame. Release/acquire pairs the
   ready flag with the constructor and the g_current publication that precede it. */
static volatile int g_smp_ready = 0;
#define SMP_READY()      __atomic_load_n(&g_smp_ready, __ATOMIC_ACQUIRE)
#define SMP_SET_READY(v) __atomic_store_n(&g_smp_ready, (v), __ATOMIC_RELEASE)

static SfSlot *sf_current_slot(void) {
    return __atomic_load_n(&g_current, __ATOMIC_ACQUIRE);
}

static void *sf_ready_object(void) {
    SfSlot *slot = sf_current_slot();
    return SMP_READY() && slot ? slot->object : NULL;
}
static long g_acb = 0, g_taskId = 0;

/* trampolines: forward library-thread events to the consumer's handlers */
static void sf_cb(int type, long long num, const char *str, void *ctx) {
    unsigned int epoch = (unsigned int)(uintptr_t)ctx;
    SfSlot *slot = sf_current_slot();
    if (type == 23 && slot && slot->epoch == epoch)
        __atomic_store_n(&slot->unload_completed, 1, __ATOMIC_RELEASE);
    sf_on_event(epoch, type, num, str);
}
static void acb_cb(long a, long t, long ev, long app, long play, const char *reply) {
    (void)a; (void)t; (void)app; (void)play;
    acb_on_event(ev, reply);
}

/* ---- StarfishMediaAPIs verbs ---- */
int sf_load(const char *payload, unsigned int epoch) {
    if (!payload || epoch == 0 ||
        __atomic_load_n(&g_lifecycle_blocked, __ATOMIC_ACQUIRE) ||
        SMP_READY() || sf_current_slot() || !sf_prepare_callback_hook()) {
        if (elogf) {
            fprintf(elogf, "native lifecycle: Load refused blocked=%d ready=%d current=%p epoch=%u\n",
                    __atomic_load_n(&g_lifecycle_blocked, __ATOMIC_ACQUIRE), SMP_READY(),
                    (void *)sf_current_slot(), epoch);
            fflush(elogf);
        }
        return 0;
    }

    SfSlot *slot = NULL;
    if (posix_memalign((void **)&slot, 16, sizeof *slot) == 0) memset(slot, 0, sizeof *slot);
    int gate_ready = slot && pthread_mutex_init(&slot->gate, NULL) == 0 &&
                     pthread_cond_init(&slot->drained, NULL) == 0;
    if (!gate_ready) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        if (elogf) {
            fprintf(elogf, "native lifecycle: Load refused (callback gate allocation/init failed)\n");
            fflush(elogf);
        }
        /* Do not recycle even an unpublished candidate: unique-address identity stays monotonic. */
        return 0;
    }
    slot->epoch = epoch;
    slot->active = 1;

    if (pthread_mutex_lock(&g_slots_lock) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        return 0;
    }
    slot->next = g_slots;
    g_slots = slot;
    if (pthread_mutex_unlock(&g_slots_lock) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        return 0;
    }
    __atomic_store_n(&g_current, slot, __ATOMIC_RELEASE);

    SMP_ctor(slot->object, NULL);   /* uid=NULL: registers on the pre-authorized uMS namespace */
    SMP_SET_READY(1);        /* RELEASE: the ctor's writes must be visible before the flag */
    SMP_notifyForeground(slot->object);
    if (pthread_mutex_lock(&slot->gate) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        SMP_SET_READY(0); /* constructed object stays quarantined; never D1/reuse */
        return 0;
    }
    slot->evidence_armed = 1;
    if (pthread_mutex_unlock(&slot->gate) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        SMP_SET_READY(0); /* constructed object stays quarantined; never D1/reuse */
        return 0;
    }
    return g_load_with_context(slot->object, payload, sf_cb, (void *)(uintptr_t)epoch);
}
int  sf_ready(void)               { return SMP_READY(); }
int sf_is_load_completed(void) {
    void *object = sf_ready_object();
    return object ? SMP_isLoadCompleted(object) : 0;
}
int sf_play(void) {
    void *object = sf_ready_object();
    return object ? SMP_Play(object) : 0;
}
int sf_pause(void) {
    void *object = sf_ready_object();
    return object ? SMP_Pause(object) : 0;
}
int sf_flush(void) {
    void *object = sf_ready_object();
    return object ? SMP_flush(object) : 0;
}
int sf_push_eos(void) {
    void *object = sf_ready_object();
    return object ? SMP_pushEOS(object) : 0;
}
void sf_unload(void) {
    void *object = sf_ready_object();
    if (object) SMP_Unload(object);
}

/* Close admission and wait without a timeout for every real hook call which already entered.
 * Synthetic UnloadCompleted bypasses callbackFunctionHook, so its independent marker is checked
 * in sf_destroy (and by Rust) rather than being confused with the interception counter. */
int sf_callback_gate_retire(void) {
    SfSlot *slot = sf_current_slot();
    if (!slot || pthread_mutex_lock(&slot->gate) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        return 0;
    }
    slot->active = 0;
    int drained = 1;
    while (slot->inflight != 0) {
        if (pthread_cond_wait(&slot->drained, &slot->gate) != 0) {
            __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
            drained = 0;
            break;
        }
    }
    slot->gate_retired = drained && slot->inflight == 0;
    unsigned int intercepted = __atomic_load_n(&slot->intercepted, __ATOMIC_RELAXED);
    unsigned int dropped = __atomic_load_n(&slot->dropped, __ATOMIC_RELAXED);
    int proven = slot->gate_retired &&
                 __atomic_load_n(&g_hook_valid, __ATOMIC_ACQUIRE) && intercepted != 0 &&
                 !__atomic_load_n(&g_lifecycle_blocked, __ATOMIC_ACQUIRE);
    if (pthread_mutex_unlock(&slot->gate) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        proven = 0;
    }
    if (elogf) {
        fprintf(elogf, "native callback gate: epoch=%u intercepted=%u dropped=%u proven=%d\n",
                slot->epoch, intercepted, dropped, proven);
        fflush(elogf);
    }
    return proven;
}

unsigned int sf_callback_intercepts(void) {
    SfSlot *slot = sf_current_slot();
    return slot ? __atomic_load_n(&slot->intercepted, __ATOMIC_RELAXED) : 0;
}

void sf_quarantine(void) {
    SfSlot *slot = sf_current_slot();
    if (slot) (void)sf_callback_gate_retire();
    __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
    SMP_SET_READY(0);
    if (elogf) {
        fprintf(elogf,
                "native lifecycle: QUARANTINED epoch=%u object=%p; D1/reuse and future Load disabled\n",
                slot ? slot->epoch : 0, slot ? (void *)slot->object : NULL);
        fflush(elogf);
    }
}

int sf_destroy(void) {
    SfSlot *slot = sf_current_slot();
    if (!slot || !SMP_READY()) return 0;
    if (pthread_mutex_lock(&slot->gate) != 0) {
        sf_quarantine();
        return 0;
    }
    int safe = !slot->active && slot->gate_retired &&
               __atomic_load_n(&slot->intercepted, __ATOMIC_RELAXED) != 0 &&
               __atomic_load_n(&slot->unload_completed, __ATOMIC_ACQUIRE) &&
               __atomic_load_n(&g_hook_valid, __ATOMIC_ACQUIRE) &&
               !__atomic_load_n(&g_lifecycle_blocked, __ATOMIC_ACQUIRE);
    if (pthread_mutex_unlock(&slot->gate) != 0) safe = 0;
    if (!safe) {
        sf_quarantine();
        return 0;
    }

    SMP_dtor(slot->object);
    __atomic_store_n(&slot->destroyed, 1, __ATOMIC_RELEASE);
    SMP_SET_READY(0);
    __atomic_store_n(&g_current, NULL, __ATOMIC_RELEASE);
    return 1;
}

/* The CustomPipeline* reached from our object (VERIFIED by decompile: StarfishMediaAPIs::
 * player is a shared_ptr _M_ptr at object+0x4c; AbstractPlayer::pipeline _M_ptr at player+0x4;
 * Pipeline is CustomPipeline's primary base so the ptr is usable as `this`). player@0x4c is
 * populated on our uid=NULL object — sf_play/sf_flush already dispatch through it. */
static void *sf_pipeline(void) {
    unsigned char *object = sf_ready_object();
    if (!object) return 0;
    void *player = *(void **)(object + 0x4c);
    if (!player) return 0;
    return *(void **)((unsigned char *)player + 0x04);
}

/* Kodi in-place seek, on the first video AU after flush(): tell the pipeline the new decode
 * timestamp, then inject a fresh GStreamer SEGMENT (the step a bare flush() omits — its
 * absence is the stale-segment stall the reload path works around). position_ns = the fed
 * (0-based rebased) PTS of that first frame. */
int sf_set_time_to_decode(long long position_ns) {
    void *object = sf_ready_object();
    if (!object) return 0;
    char j[64];
    snprintf(j, sizeof j, "{\"position\":%lld}", position_ns);
    return SMP_setTimeToDecode(object, j);
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
    void *object = sf_ready_object();
    if (!object) return 'e';
    char j[160];
    snprintf(j, sizeof j, "{\"bufferAddr\":\"%p\",\"bufferSize\":%u,\"pts\":%lld,\"esData\":%d}",
             (const void *)p, size, pts, esData);
    unsigned char ret[32];
    memset(ret, 0, sizeof ret);
    SMP_Feed(ret, object, j);
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
    /* NO FALLBACK ID. This used to substitute the shipped app's literal id for a NULL `appId`,
     * which was the far half of a double fallback: the Rust side read `getenv("APPID")` and passed
     * NULL when SAM had not exported it, and this line then quietly claimed to be the app users
     * install. With a developer build able to sit beside that app on one television, a wrong id
     * here binds the video plane for the WRONG application — audio with a black plane, no error
     * line anywhere. The near half is fixed (`engine::acb_init_acb` now passes
     * `paths::app_id()`, read from the install directory, which cannot be absent); refusing here
     * rather than guessing is what keeps the pair readable together. */
    if (g_acb) {
        if (appId) {
            acb.initialize(g_acb, playerType, appId, acb_cb);
        } else {
            /* AND HAND BACK NOTHING. Returning the live handle here made the refusal half-done:
             * the Rust side stores `ACB_OK = acb != 0`, so `acb_bind`, `acb_send_video_data` and
             * `acb_send_atmos` would all then run against an object the library was never told
             * about — strictly worse than the fallback id this replaced, which at least talked to
             * an initialized ACB. Zeroing it makes every `if (!g_acb) return;` guard below do its
             * job. The handle itself is leaked because this build resolves no `AcbAPI_destroy`
             * (see the dlsym table's note); one leaked handle on a path that cannot be reached —
             * `app_id()` cannot be absent — is the right trade against calling into uninitialized
             * memory. */
            if (elogf) {
                fprintf(elogf, "acb: no appId — NOT initialized and NOT returned; video will not bind\n");
                fflush(elogf);
            }
            g_acb = 0;
        }
    }
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
