/* starfish.c — buffer-feed StarfishMediaAPIs C++ ABI seam; see starfish.h.
 * Every C++-ABI hazard lives here: the mangled __asm__ symbols, Feed's sret
 * std::string and the over-allocated in-place object (never new/delete). The
 * display binding lives separately in video_plane.c. Callers touch flat sf_ verbs. */
#include "starfish.h"
#include <dlfcn.h>
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern FILE *elogf;   /* the event log (owned by the boot shim) */

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
/* Feed returns std::string, so its mangled name carries the library's string ABI: `B5cxx11` on
 * the 4.x inventories and later (4.4.2 is the oldest graded with it), the pre-C++11
 * copy-on-write string on 3.9.2 and earlier (`_ZN17StarfishMediaAPIs4FeedEPKc`). Either way the hidden sret slot starts with the char
 * pointer, so the reply reads the same; only the COW string must be released through the
 * firmware's own destructor (`_ZNSsD1Ev`), because its buffer is a refcounted heap rep. Both are
 * resolved at runtime: a strong reference to the cxx11 name is a lazily-bound PLT slot that kills
 * the process at the first Feed on 3.4.0 (tools/fwcompat.py --release 3.4.0). */
typedef void (*SMP_FeedFn)(void *sret, void *self, const char *payload);
typedef void (*CowStringDtorFn)(void *str);
static SMP_FeedFn g_feed;
static CowStringDtorFn g_feed_reply_dtor;
/* Set when the COW name is the one that resolved. Kept apart from g_feed_reply_dtor because that
 * can be NULL on a COW library (`_ZNSsD1Ev` missing), and the ABI is then still COW. */
static int g_feed_cow;
static pthread_once_t g_feed_once = PTHREAD_ONCE_INIT;
static void sf_resolve_feed_once(void) {
    g_feed = (SMP_FeedFn)dlsym(RTLD_DEFAULT, "_ZN17StarfishMediaAPIs4FeedB5cxx11EPKc");
    if (!g_feed) {
        g_feed = (SMP_FeedFn)dlsym(RTLD_DEFAULT, "_ZN17StarfishMediaAPIs4FeedEPKc");
        if (g_feed) {
            g_feed_cow = 1;
            g_feed_reply_dtor = (CowStringDtorFn)dlsym(RTLD_DEFAULT, "_ZNSsD1Ev");
        }
    }
    if (elogf) {
        /* cow with dtor=(nil) means every Feed reply leaks its string rep. */
        fprintf(elogf, "feed abi: %s fn=%p dtor=%p\n",
                !g_feed ? "NONE" : g_feed_cow ? "cow" : "cxx11",
                (void *)g_feed, (void *)g_feed_reply_dtor);
        fflush(elogf);
    }
}
extern int  SMP_Play(void *self) __asm__("_ZN17StarfishMediaAPIs4PlayEv");
extern int  SMP_Unload(void *self) __asm__("_ZN17StarfishMediaAPIs6UnloadEv");
extern void SMP_notifyForeground(void *self) __asm__("_ZN17StarfishMediaAPIs16notifyForegroundEv");
extern int  SMP_isLoadCompleted(void *self) __asm__("_ZN17StarfishMediaAPIs15isLoadCompletedEv");
extern int  SMP_Pause(void *self) __asm__("_ZN17StarfishMediaAPIs5PauseEv");
extern int  SMP_flush(void *self) __asm__("_ZN17StarfishMediaAPIs5flushEv");
/* LG's PUBLIC seek (#568): bool Seek(const char *millis). A decimal-milliseconds string; a negative
 * value is refused. Present on every firmware 1.2-11.2 (tools/fwcompat.py). Decompiled on 4.5:
 * it parses the argument, takes the Play() mutex and runs a FLUSH|KEY_UNIT gst_element_seek whose
 * segment starts at the millis passed - no private offset, no decoder re-init. */
extern int  SMP_Seek(void *self, const char *millis) __asm__("_ZN17StarfishMediaAPIs4SeekEPKc");
/* Per-pipeline output volume (the paused-preroll mute): bool setVolume(const char *json). Present on
 * every firmware 4.4.2-11.2.0 (fwsym inventory) and declared in the NDK header. Decompiled on 4.x:
 * setVolume parses {"volume":<int>} and, without an "ease" block, applies it at once; its bool
 * proves nothing (it returns 1 for a null element). Not setAudioMute (absent on 4.4.2 / 4.10.0).
 *
 * getVolume is DELIBERATELY NOT BOUND. The NDK header says `const char *getVolume()`, but the 4.5
 * libplayerAPIs tail-calls the pipeline's vtable slot 0xc8 (`ldr r0,[r0,#0x4c]; ldr r3,[r0];
 * ldr r3,[r3,#0xc8]; bx r3`), and libpf's implementations (mediapipeline::AbstractPlayer::getVolume,
 * CustomPipeline::getVolume) return a plain int: the g_object_get "volume" value, 100 when the
 * property is absent, -1 from the base class. Reading that int as a char* crashed the TV (SIGSEGV,
 * r0=0x64 into strstr). The mangled name carries no return type and only one firmware was
 * decompiled, so no readback is declared: the preroll restores the pipeline default (100). */
extern int  SMP_setVolume(void *self, const char *json) __asm__("_ZN17StarfishMediaAPIs9setVolumeEPKc");
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

/* issue #74: SMP_READY() only ever meant "an object exists" — it goes true at :475, BEFORE the
   real, synchronous g_load_with_context() call below returns, and every verb dispatches through
   sf_ready_object() alone. On a chassis where that call blocks for a long time (k5lp video-output
   init), the main thread was free to Feed/Play/etc. into an object whose LoadCommon ctor path was
   still running on the load thread — a null-arg SIGSEGV on one path, a Play-vs-LoadCommon mutex
   deadlock ending in an OMX_VideoComp abort on the other. g_load_returned closes that window
   without changing what SMP_READY()/sf_ready() answer: cleared the instant the real Load call is
   about to be made (paired with the sites below that set SMP_READY(0)), set back once it returns.
   No locks — the same __atomic RELEASE/ACQUIRE pairing SMP_READY() already uses. */
static volatile int g_load_returned = 0;
#define LOAD_RETURNED()      __atomic_load_n(&g_load_returned, __ATOMIC_ACQUIRE)
#define SET_LOAD_RETURNED(v) __atomic_store_n(&g_load_returned, (v), __ATOMIC_RELEASE)

static SfSlot *sf_current_slot(void) {
    return __atomic_load_n(&g_current, __ATOMIC_ACQUIRE);
}

static void *sf_ready_object(void) {
    SfSlot *slot = sf_current_slot();
    return SMP_READY() && LOAD_RETURNED() && slot ? slot->object : NULL;
}

/* trampolines: forward library-thread events to the consumer's handlers */
static void sf_cb(int type, long long num, const char *str, void *ctx) {
    unsigned int epoch = (unsigned int)(uintptr_t)ctx;
    SfSlot *slot = sf_current_slot();
    if (type == 23 && slot && slot->epoch == epoch)
        __atomic_store_n(&slot->unload_completed, 1, __ATOMIC_RELEASE);
    sf_on_event(epoch, type, num, str);
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
    /* Close the dispatch window BEFORE opening it: clear g_load_returned first, then set ready.
       Today the two writes cannot race anything — every SMP_SET_READY(0) site (below, and the
       other three in this file) already clears g_load_returned too, so it is provably 0 here
       already — but this ordering makes that a LOCAL invariant instead of one that depends on
       every future SMP_SET_READY(1) call site remembering to clear first. */
    SET_LOAD_RETURNED(0);    /* the real Load call below hasn't happened yet: not dispatchable */
    SMP_SET_READY(1);        /* RELEASE: the ctor's writes must be visible before the flag */
    SMP_notifyForeground(slot->object);
    if (pthread_mutex_lock(&slot->gate) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        SMP_SET_READY(0); /* constructed object stays quarantined; never D1/reuse */
        SET_LOAD_RETURNED(0);
        return 0;
    }
    slot->evidence_armed = 1;
    if (pthread_mutex_unlock(&slot->gate) != 0) {
        __atomic_store_n(&g_lifecycle_blocked, 1, __ATOMIC_RELEASE);
        SMP_SET_READY(0); /* constructed object stays quarantined; never D1/reuse */
        SET_LOAD_RETURNED(0);
        return 0;
    }
    /* THE actual Load: synchronous on this thread (the load thread — see threads.rs), and on some
       chassis it blocks for a long time inside video-output init (issue #74). g_load_returned
       stays 0 — "in flight" — for the whole of this call; every other verb refuses to dispatch
       until it flips back to 1, which happens before the result is propagated below. */
    int ok = g_load_with_context(slot->object, payload, sf_cb, (void *)(uintptr_t)epoch);
    SET_LOAD_RETURNED(1);
    return ok;
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
/* Set this pipeline's volume (0..100). Returns the library's bool; 0 also when nothing is loaded. */
int sf_set_volume(int volume) {
    void *object = sf_ready_object();
    if (!object) return 0;
    char json[32];
    snprintf(json, sizeof json, "{\"volume\":%d}", volume);
    return SMP_setVolume(object, json) & 0xff;   /* bool in r0: only the low byte is defined */
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
    SET_LOAD_RETURNED(0);
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
    SET_LOAD_RETURNED(0);
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
/* Public in-place seek (#568): the pipeline flushes itself and re-anchors its segment at `ms`, so
 * the caller must feed PTS on that same timeline afterwards and must NOT call sf_flush() (a flush is
 * itself a seek to 0). Not safe against a concurrent sf_feed - the caller quiesces the feeder.
 * Returns the library's bool: 0 = refused (negative ms, not loaded, or failed inside the pipeline,
 * which then rejects Feed until a seek/flush succeeds). */
int sf_seek_ms(long long ms) {
    void *object = sf_ready_object();
    if (!object) return 0;
    char arg[32];
    snprintf(arg, sizeof arg, "%lld", ms);
    int rv = SMP_Seek(object, arg) & 0xff;   /* bool in r0: only the low byte is defined */
    if (elogf) { fprintf(elogf, "Seek(%s) -> %d\n", arg, rv); fflush(elogf); }
    return rv;
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
    pthread_once(&g_feed_once, sf_resolve_feed_once);
    if (!g_feed) return 'e';
    char j[160];
    snprintf(j, sizeof j, "{\"bufferAddr\":\"%p\",\"bufferSize\":%u,\"pts\":%lld,\"esData\":%d}",
             (const void *)p, size, pts, esData);
    unsigned char ret[32];
    memset(ret, 0, sizeof ret);
    g_feed(ret, object, j);
    char *s = *(char **)ret;             /* std::string _M_p at offset 0 */
    static int logged = 0;
    if (elogf && logged < 3) { logged++;
        fprintf(elogf, "feed reply=\"%s\"\n", s ? s : "(null)"); fflush(elogf); }
    char verdict = !s ? 'e' : strstr(s, "BufferFull") ? 'B' : strstr(s, "Ok") ? 'O' : 'e';
    /* The COW destructor reads the rep header just below _M_p, so a NULL _M_p (Feed returned
       without constructing its result) would fault. */
    if (s && g_feed_reply_dtor) g_feed_reply_dtor(ret);
    return verdict;
}

/* 1 when the library's Feed is the pre-C++11 copy-on-write build: direct evidence that its
   StarfishMediaAPIs layout is not the 4.5 one the in-place seek offsets were decompiled from. */
int sf_feed_abi_is_cow(void) {
    pthread_once(&g_feed_once, sf_resolve_feed_once);
    return g_feed_cow;
}
