/* starfish.h — the low-level C subsystem: LG StarfishMediaAPIs (C++) + ACB
 * video-plane binding, behind flat C verbs. Hides the 11 mangled __asm__ symbols,
 * the sret std::string in Feed, the never-reused 64KB in-place objects, and the 3-arg ACB
 * taskId ABI. Library-thread callbacks are forwarded to sf_on_event/acb_on_event,
 * which the Rust player engine defines (player/mod.rs). This is the one piece that
 * stays C in the Rust-first app — porting the mangled-C++ FFI to Rust is
 * worse-than-C. */
#ifndef PLXNATIVE_STARFISH_H
#define PLXNATIVE_STARFISH_H

#define PLAYER_TYPE_MSE 10   /* ACB playerType (default) */

/* ---- StarfishMediaAPIs pipeline ---- */
int  sf_load(const char *payload, unsigned int epoch); /* Load callback context owns `epoch` */
/* 1 while the current object is dispatchable; 0 can retain a quarantined object. */
int  sf_ready(void);
int  sf_is_load_completed(void);
int  sf_play(void);
int  sf_pause(void);
int  sf_flush(void);
int  sf_set_time_to_decode(long long position_ns); /* Kodi in-place seek: setTimeToDecode (returns 0 on webOS<11) */
int  sf_set_content_info(long long position_ns);   /* webOS<11 in-place seek: loadSpi_getInfo + setContentInfo(ptsToDecode); 0 = pipeline not reachable */
int  sf_send_segment(void);                        /* Kodi in-place seek: CustomPipeline::sendSegmentEvent; 0 = pipeline not reachable */
char sf_feed(const unsigned char *p, unsigned size, long long pts, int esData); /* 'O'/'B'/'e' */
void sf_unload(void);                /* Unload the pipeline */
/* After Unload: close native callback admission and wait (without a timeout) for every admitted
 * callbackFunctionHook call. 1 proves this object's ELF interposer was actually crossed. */
int  sf_callback_gate_retire(void);
unsigned int sf_callback_intercepts(void); /* runtime evidence counter for the current object */
/* D1 is permitted only after gate retirement + intercepted callback + synchronous type 23. The C
 * seam rechecks all three and quarantines instead of destructing if any proof is absent. */
int  sf_destroy(void);
/* Keep the current constructed object forever and permanently reject another Load. */
void sf_quarantine(void);

/* ---- the video-plane binding: decoded sink -> display plane ----
 *
 * Two mechanisms, never both, split at exactly webOS 5.0 where LG deleted libAcbAPI and gave SDL
 * an exported-window API instead. `vp_mode` resolves which this television has, once, by trying to
 * dlopen/dlsym each — neither is linked, because naming a library or symbol the device lacks makes
 * the loader kill the process before main. See the long comment at the top of starfish.c. */
#define VP_NONE     0   /* neither: video cannot be displayed, but the app still runs */
#define VP_ACB      1   /* webOS 2.2.3 .. 4.10.0 — libAcbAPI.so.1 */
#define VP_EXPORTED 2   /* webOS 5.3.1 .. 11.2.0 — SDL_webOS*ExportedWindow* (device-verified webOS 6.5.2, issue #22) */
int vp_mode(void);

/* VP_EXPORTED only. Create the exported window and return its compositor-assigned id, or NULL.
 * MUST be called before Load: the id travels inside the Load payload as option.windowId, and on
 * webOS 5 that string is the entire binding. The returned pointer is owned by this seam. */
const char *vp_create_window(void);
/* VP_EXPORTED counterpart of acb_start's setDisplayWindow: source frame size -> on-screen rect. */
/* The exported windowId we hold, or "" when none was created. Diagnostics only; never NULL. */
const char *vp_window_id(void);
int  vp_place(int src_w, int src_h, int dst_x, int dst_y, int dst_w, int dst_h);
void vp_destroy_window(void);

/* ACB (App Common Binding) — VP_ACB only; every verb is a no-op in the other modes, so callers
 * need not branch. On webOS 5 this whole sequence has no replacement: it is deleted outright. */
long acb_create(const char *appId, int playerType);        /* create+initialize; 0 = failed */
void acb_bind(const char *mediaId);                        /* setSinkType(MAIN)+setMediaId+setState(LOADED) */
int  acb_send_video_data(const char *sourceInfoVerbatim);  /* setMediaVideoData; -1 = rejected */
void acb_start(long x, long y, long w, long h);            /* setDisplayWindow + setState(PLAYING) */
void acb_unload(void);                                     /* setState(UNLOADED) */

/* ---- library-thread callbacks the seam forwards to (consumer-defined) ---- */
void sf_on_event(unsigned int epoch, int type, long long num, const char *str);
void acb_on_event(long ev, const char *reply);

#endif /* PLXNATIVE_STARFISH_H */
