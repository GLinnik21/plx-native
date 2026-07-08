/* starfish.h — the low-level C subsystem: LG StarfishMediaAPIs (C++) + ACB
 * video-plane binding, behind flat C verbs. Hides the 11 mangled __asm__ symbols,
 * the sret std::string in Feed, the 64KB in-place object, and the 3-arg ACB
 * taskId ABI. Library-thread callbacks are forwarded to sf_on_event/acb_on_event,
 * which the consumer defines (C today; Rust once the engine is ported). This is
 * the one piece that stays C in the Rust-first app — porting the mangled-C++ FFI
 * to Rust is worse-than-C. */
#ifndef PLEXPOC_STARFISH_H
#define PLEXPOC_STARFISH_H

#define PLAYER_TYPE_MSE 10   /* ACB playerType (default) */

/* ---- StarfishMediaAPIs pipeline ---- */
int  sf_load(const char *payload);   /* ctor(uid=NULL) + notifyForeground + Load */
int  sf_ready(void);                 /* 1 once the in-place pipeline object exists */
int  sf_is_load_completed(void);
int  sf_play(void);
int  sf_pause(void);
int  sf_flush(void);
void sf_set_playtime(long long t);
int  sf_set_time_to_decode(long long position_ns); /* Kodi in-place seek: setTimeToDecode */
int  sf_send_segment(void);                        /* Kodi in-place seek: CustomPipeline::sendSegmentEvent; 0 = pipeline not reachable */
char sf_feed(const unsigned char *p, unsigned size, long long pts, int esData); /* 'O'/'B'/'e' */
void sf_unload(void);                /* Unload the pipeline */
void sf_destroy(void);               /* destruct the object; clears sf_ready */

/* ---- ACB (App Common Binding): decoded video sink -> display plane ---- */
long acb_create(const char *appId, int playerType);        /* create+initialize; 0 = failed */
void acb_bind(const char *mediaId);                        /* setSinkType(MAIN)+setMediaId+setState(LOADED) */
int  acb_send_video_data(const char *sourceInfoVerbatim);  /* setMediaVideoData; -1 = rejected */
void acb_start(long x, long y, long w, long h);            /* setDisplayWindow + setState(PLAYING) */
void acb_unload(void);                                     /* setState(UNLOADED) */
void acb_destroy(void);                                    /* finalize + destroy */

/* ---- library-thread callbacks the seam forwards to (consumer-defined) ---- */
void sf_on_event(int type, long long num, const char *str);
void acb_on_event(long ev, const char *reply);

#endif /* PLEXPOC_STARFISH_H */
