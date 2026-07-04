#ifndef PLEXPOC_POSTERS_H
#define PLEXPOC_POSTERS_H
/* Async poster / artwork texture store for the gallery.
 *
 * MAIN (GL) thread:
 *   poster_get(key)  -> READY texture, or 0 (and enqueues a fetch on a miss)
 *   poster_pump(n)   -> upload up to n decoded slots (glTexImage2D); call once/frame
 *   posters_init/shutdown
 * 2 BACKGROUND workers: HTTP GET (own-stack http_stream) + stb decode to RGBA.
 *   Workers make NO gl* calls — the GLES context is main-thread-only.
 *
 * Handoff crosses the thread boundary via one mutex+cond; every slot carries a
 * `gen` bumped on eviction so a late decode into a recycled slot is discarded.
 * The store is INDEPENDENT of the playback pipeline: stop_bufferfeed() must never
 * touch it. Implementation in posters.c. */
#include <GLES2/gl2.h>
#include <stddef.h>   /* size_t (poster_key proto) */

void   posters_init(const char *host, int port, const char *token);
void   posters_shutdown(void);
/* READY texture for key, else 0 (claim a slot + enqueue fetch on miss). MAIN thread. */
GLuint poster_get(const char *key);
/* pixel dims of a READY texture for key (0,0 if not ready). Does NOT trigger a fetch. */
void   poster_wh(const char *key, int *w, int *h);
/* MAIN/GL thread, once per frame BEFORE drawing. Uploads up to `budget` decoded slots. */
void   poster_pump(int budget);
/* Build the transcode request path (also the store key). png=1 -> transparent clearLogo. */
void   poster_key(char *dst, size_t cap, const char *src_path, int w, int h, int png);

#endif /* PLEXPOC_POSTERS_H */
