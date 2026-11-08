/* Shared native display-plane binding for URI and buffer-feed playback.
 * See starfish.h for the existing flat display ABI. Feed-specific native objects and
 * callback admission remain in starfish.c. */
#include "starfish.h"
#include <dlfcn.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern FILE *elogf;

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
     * pointers here that may legitimately be NULL (connectDass is optional too). `AcbAPI_setMediaAudioData` is exported on
     * webOS 3.9.2 / 4.4.2 / 4.10.0 but NOT on 2.2.3 or 3.4.0 (tools/fwcompat.py --lib
     * libAcbAPI.so.1.0.0 --grep setMedia). Adding it to the AND would refuse ACB outright on
     * those two releases and take ALL VIDEO with it — a Dolby Atmos read-out is not worth a
     * black screen. See acb_send_atmos(). */
    int  (*setMediaAudioData)(long, const char *, long *);
    /* Optional experiment, never part of the required ACB gate. Device SHA256
     * d9627a2f7e2c5efe8ab562b94fa6640b93d01da6e6ba41654aef69e22b24af1b:
     * AcbAPI_connectDass@0x16720 forwards mode+1 and long* task to core@0x10a14.
     * External zero selects sinkTypeList[1]="main"; only that proven mode is exposed. */
    int  (*connectDass)(long, int, long *);
    /* Probe-only observation of the existing boot initialization; BF behavior is unchanged. */
    int probe_initialize_result;
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
        acb.connectDass       = dlsym(h, "AcbAPI_connectDass");
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

/* `g_acb` is the handle every ACB verb below addresses. The boot may create two (one per backend:
 * `tv::plane::PlaneHandle`, Feed's and the URI engine's, each created and initialized once and never
 * destroyed); `acb_select` moves `g_acb` between them and only the plane owner calls it, on the
 * grant of a bind, so exactly the bound handle is current. A slot keeps the library's answer to its
 * own `AcbAPI_initialize`, which `acb_initialized` reports for the current handle. */
#define ACB_SLOTS 2
static struct { long handle; int init_rv; } g_slots[ACB_SLOTS];
static int g_nslots = 0;
static long g_acb = 0, g_taskId = 0;

static void acb_cb(long a, long t, long ev, long app, long play, const char *reply) {
    /* `a` is the long `AcbAPI_create` returned for the core that issued `t`: AcbAPI_initialize binds
     * its own handle argument as the callback's first parameter (libAcbAPI 0x15c4c, the bound
     * std::function invoker 0x18858; /Volumes/DAS/lgfw/reports/acb-callback-id/report.md). Each core
     * numbers its tasks from its own counter, so with two handles a task id means something only
     * together with `a`. The trailing `id=` keeps the line's historical prefix for the harness. */
    if (elogf) { fprintf(elogf, "acb callback task=%ld ev=%ld app=%ld play=%ld id=%ld\n", t, ev, app, play, a); fflush(elogf); }
    /* `t` is the task id the originating call wrote; the Rust mailbox matches the UNLOADED fence on (a, t). */
    acb_on_event(a, t, ev, reply);
}

/* ---- ACB verbs (the 3-arg taskId ABI is hidden) ----
 *
 * Every one is a no-op when this device has no ACB, so the caller does not have to branch: on
 * webOS 5 the whole setSinkType / setMediaId / setMediaVideoData / setState sequence has no
 * replacement — it is simply deleted, which is what both reference implementations do (ss4s stubs
 * all of them to `return true`, Kodi guards each with `if (acb)`).
 *
 * issue #74 D.1: these dispatch on `g_acb` alone and never call `sf_ready_object()`, so the
 * in-flight-Load gate in starfish.c does NOT cover them directly — unlike the nine `sf_*` verbs above.
 * They are today bounded only indirectly, by `pump.rs`'s stage machine (every ACB call site
 * that binds or updates media requires `eng.stage >= Stage::Playing`, reached only after
 * the pump has observed Load return (this file's loadCompleted arm having already observed
 * `LOAD_RETURNED()`) — initialization and teardown are separate lifecycle paths. Extending the
 * gate itself into ACB
 * would need the same device verification any Starfish/ACB bind-order change needs (see
 * player/CLAUDE.md); this comment exists so "the gate covers every Starfish/ACB verb" is not
 * read as true of this block until that is done. */
long acb_create(const char *appId, int playerType) {
    if (vp_mode() != VP_ACB) return 0;
    if (g_nslots >= ACB_SLOTS) {
        if (elogf) { fprintf(elogf, "acb create playerType=%d refused: both slots are taken\n", playerType); fflush(elogf); }
        return 0;
    }
    /* A new handle replaces the current one only once it exists (create answered non-zero and an
     * appId was given): a failed second create must not take the first's `g_acb` away. Whether its
     * `initialize` was accepted is kept in its slot (`acb_initialized`), as it always was: a created
     * handle whose initialize answered 0 is still the historical return. */
    long handle = acb.create();
    int init_rv = 0;
    /* NO FALLBACK ID. This used to substitute the shipped app's literal id for a NULL `appId`,
     * which was the far half of a double fallback: the Rust side read `getenv("APPID")` and passed
     * NULL when SAM had not exported it, and this line then quietly claimed to be the app users
     * install. With a developer build able to sit beside that app on one television, a wrong id
     * here binds the video plane for the WRONG application — audio with a black plane, no error
     * line anywhere. The near half is fixed (`acb_init_acb` in the app's
     * `src/app/adapters/backends.rs` now passes `paths::app_id()`, read from the install
     * directory, which cannot be absent); refusing here
     * rather than guessing is what keeps the pair readable together. */
    if (handle) {
        if (appId) {
            init_rv = acb.initialize(handle, playerType, appId, acb_cb);
            if (elogf) { fprintf(elogf, "acb initialize playerType=%d rv=%d callback_installed=%d\n", playerType, init_rv, init_rv == 1); fflush(elogf); }
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
            handle = 0;
        }
    }
    if (!handle) {
        /* The first create keeps the historical answer: no handle at all. A later one leaves the
         * handle already held alone. */
        if (g_nslots == 0) { g_acb = 0; acb.probe_initialize_result = 0; }
        return 0;
    }
    g_slots[g_nslots].handle = handle;
    g_slots[g_nslots].init_rv = init_rv;
    g_nslots++;
    g_acb = handle;
    acb.probe_initialize_result = init_rv;
    return handle;
}
/* Make `handle` (a value `acb_create` returned) the one every verb below addresses. 1 = it is one of
 * this process's boot handles and now current, 0 = unknown, nothing changed. Only the plane owner
 * calls it, when it grants a bind: one binding at a time means the current handle IS the bound one. */
int acb_select(long handle) {
    for (int i = 0; i < g_nslots; i++) {
        if (g_slots[i].handle != handle) continue;
        if (g_acb != handle && elogf) { fprintf(elogf, "acb select handle=%ld\n", handle); fflush(elogf); }
        g_acb = handle;
        acb.probe_initialize_result = g_slots[i].init_rv;
        return 1;
    }
    return 0;
}
/* The URI bind (the production backend and the developer probe, both through play-uri's
 * `acb_bind`) uses its own process-lifetime boot handle (player type 0, selected by the plane owner)
 * and the same callback as BF. These split the existing bind solely to preserve observed inter-call
 * delays. */
/* Same device SHA pinned above: initialize wrapper@0x15c4c forwards core@0xcd5c.
 * Core returns exactly 1 only after valid parameters and callback installation; 0 refuses.
 * Answers for the CURRENT handle, so it is only meaningful at the boot right after that handle's
 * acb_create: the boot reads it once per handle into the plane owner, which the URI gate reads
 * afterwards. normal acb_create still returns its historical handle result. */
int acb_initialized(void) { return g_acb && acb.probe_initialize_result == 1; }
int acb_probe_dass_available(void) { return g_acb && acb.connectDass; }
int acb_media_id(const char *mediaId) {
    if (!g_acb || !mediaId) return 0;
    int sink = acb.setSinkType(g_acb, SINK_TYPE_MAIN);
    int rv = acb.setMediaId(g_acb, mediaId);
    if (elogf) { fprintf(elogf, "acb probe mediaId sink_rv=%d rv=%d\n", sink, rv); fflush(elogf); }
    return sink == 1 && rv == 1;
}
int acb_loaded(void) {
    if (!g_acb) return 0;
    int rv = acb.setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_LOADED, &g_taskId);
    if (elogf) { fprintf(elogf, "acb probe loaded rv=%d task=%ld\n", rv, g_taskId); fflush(elogf); }
    return rv;
}
int acb_probe_connect_dass(void) {
    if (!g_acb || !acb.connectDass) return 0;
    int rv = acb.connectDass(g_acb, 0, &g_taskId);
    if (elogf) { fprintf(elogf, "acb probe connectDass mode=0 rv=%d task=%ld\n", rv, g_taskId); fflush(elogf); }
    return rv;
}
void acb_bind(const char *mediaId) {
    (void)acb_media_id(mediaId);
    (void)acb_loaded();
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
int acb_send_video_data(const char *sourceInfo) {
    if (!g_acb) return 0;
    int rv = acb.setMediaVideoData(g_acb, sourceInfo, &g_taskId);
    if (elogf) { fprintf(elogf, "acb videoData rv=%d task=%ld\n", rv, g_taskId); fflush(elogf); }
    return rv;
}
/* setDisplayWindow + setState(PLAYING). `displayTask` / `playingTask` (either may be NULL) receive
 * the task id the firmware will complete each call with, ONLY when that call answered 1, else 0:
 * the plane owner matches the two completions on them (tv::plane, `Shown`). Both calls still hand
 * the library &g_taskId, as they always have; an accepted call has just written it (the id is
 * created before the call returns), so it is copied out at once, before the next call reuses it. */
static int acb_start_impl(long x, long y, long w, long h, int strict, long *displayTask, long *playingTask) {
    if (displayTask) *displayTask = 0;
    if (playingTask) *playingTask = 0;
    if (!g_acb) return 0;
    int display = acb.setDisplayWindow(g_acb, x, y, w, h, 1, &g_taskId);
    if (elogf) { fprintf(elogf, "acb display fullscreen=1 rv=%d task=%ld\n", display, g_taskId); fflush(elogf); }
    if (display == 1 && displayTask) *displayTask = g_taskId;
    if (strict && display != 1) return 0;
    int playing = acb.setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_PLAYING, &g_taskId);
    if (elogf) { fprintf(elogf, "acb playing rv=%d task=%ld\n", playing, g_taskId); fflush(elogf); }
    if (playing == 1 && playingTask) *playingTask = g_taskId;
    return display == 1 && playing == 1;
}
void acb_start(long x, long y, long w, long h) { (void)acb_start_impl(x, y, w, h, 0, NULL, NULL); }
int acb_finish(long *displayTask, long *playingTask) {
    return acb_start_impl(0, 0, 1280, 720, 1, displayTask, playingTask);
}
int acb_unload(long *taskOut) {
    if (taskOut) *taskOut = 0;
    if (!g_acb) return 0;
    /* Its own slot, zeroed before the call, not g_taskId: setState writes nothing on -2 (not
     * registered), and g_taskId would then still hold the pause / resume / video-data task of an
     * earlier call. Static rather than a stack local so a library build that stored the id after
     * returning (only the dev set's libAcbAPI is read: docs/measurements/acb-release-fence-2026-11-04.md)
     * could not write into a dead frame. */
    static long unload_task;
    unload_task = 0;
    int rv = acb.setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_UNLOADED, &unload_task);
    if (elogf) { fprintf(elogf, "acb unloaded rv=%d task=%ld\n", rv, unload_task); fflush(elogf); }
    if (rv == 1 && taskOut) *taskOut = unload_task;
    return rv;
}
/* Kodi-parity: mirror the ACB PLAYSTATE on transport pause/resume (the app owns the sink; the
 * pipeline Pause/Play alone leaves the ACB state stale). Only meaningful once the plane is bound. */
void acb_pause(void)  { if (g_acb) acb.setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_PAUSED,  &g_taskId); }
void acb_resume(void) { if (g_acb) acb.setState(g_acb, APPSTATE_FOREGROUND, PLAYSTATE_PLAYING, &g_taskId); }
