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
 * `gen` bumped on eviction so a late decode into a recycled slot is discarded
 * (never wrong-image, never double-free). The store is INDEPENDENT of the
 * playback pipeline: stop_bufferfeed() must never touch it, so returning from a
 * movie shows the posters instantly. Textures are deleted only in poster_get
 * eviction and posters_shutdown (both main thread). */
#include <pthread.h>
#include <time.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include "stream.h"
#include "img.h"

#define PT_CAP 64
enum { P_EMPTY, P_WANT, P_LOADING, P_DECODED, P_UPLOADING, P_READY, P_FAILED };

typedef struct {
    char           key[256];   /* full /photo/:/transcode request path = store key */
    GLuint         tex;
    int            pw, ph;
    unsigned char *px;         /* decoded RGBA; owned by worker until published, then main */
    volatile int   state;
    unsigned       use;        /* LRU clock */
    unsigned       gen;        /* bumped on eviction; stale-decode guard */
    unsigned       frame;      /* last frame poster_get touched it (evict-protect) */
} pslot;

static pslot            g_ptex[PT_CAP];
static pthread_mutex_t  g_ptex_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t   g_ptex_cv = PTHREAD_COND_INITIALIZER;
static unsigned         g_ptex_clock = 0;
static unsigned         g_ptex_frame = 0;
static volatile int     g_poster_quit = 0;
static pthread_t        g_pworkers[2];
static int              g_pworkers_n = 0;
static const char      *g_pms_host = NULL;
static int              g_pms_port = 0;
static const char      *g_pms_token = NULL;

/* percent-encode a Plex server-relative path for the transcode url= query value */
static void urlenc(char *dst, size_t cap, const char *src) {
    static const char *hex = "0123456789ABCDEF";
    size_t o = 0;
    for (; *src && o + 4 < cap; src++) {
        unsigned char ch = (unsigned char)*src;
        if ((ch >= 'A' && ch <= 'Z') || (ch >= 'a' && ch <= 'z') ||
            (ch >= '0' && ch <= '9') || ch == '-' || ch == '_' || ch == '.' || ch == '~')
            dst[o++] = (char)ch;
        else { dst[o++] = '%'; dst[o++] = hex[ch >> 4]; dst[o++] = hex[ch & 15]; }
    }
    dst[o] = 0;
}

/* Build the transcode request path (also the store key). png=1 -> transparent clearLogo. */
static void poster_key(char *dst, size_t cap, const char *src_path, int w, int h, int png) {
    char enc[512]; urlenc(enc, sizeof enc, src_path);
    snprintf(dst, cap, "/photo/:/transcode?width=%d&height=%d&minSize=1&url=%s%s&X-Plex-Token=%s",
             w, h, enc, png ? "&format=png" : "", g_pms_token ? g_pms_token : "");
}

/* MAIN thread. READY texture for key, else 0 (claim a slot + enqueue fetch on miss). */
static GLuint poster_get(const char *key) {
    pthread_mutex_lock(&g_ptex_mu);
    for (int i = 0; i < PT_CAP; i++) {
        if (g_ptex[i].state != P_EMPTY && strcmp(g_ptex[i].key, key) == 0) {
            g_ptex[i].use = ++g_ptex_clock;
            g_ptex[i].frame = g_ptex_frame;
            GLuint t = (g_ptex[i].state == P_READY) ? g_ptex[i].tex : 0;
            pthread_mutex_unlock(&g_ptex_mu);
            return t;
        }
    }
    /* miss: prefer an EMPTY slot, else LRU-evict a settled slot not used this frame */
    int slot = -1; unsigned oldest = ~0u;
    for (int i = 0; i < PT_CAP; i++)
        if (g_ptex[i].state == P_EMPTY) { slot = i; break; }
    if (slot < 0)
        for (int i = 0; i < PT_CAP; i++)
            if ((g_ptex[i].state == P_READY || g_ptex[i].state == P_FAILED) &&
                g_ptex[i].frame != g_ptex_frame && g_ptex[i].use < oldest) {
                oldest = g_ptex[i].use; slot = i;
            }
    if (slot < 0) { pthread_mutex_unlock(&g_ptex_mu); return 0; }  /* all visible: skip */
    pslot *s = &g_ptex[slot];
    if (s->tex) { glDeleteTextures(1, &s->tex); s->tex = 0; }   /* MAIN-thread GL */
    if (s->px)  { img_free(s->px); s->px = NULL; }              /* drop pending decode */
    s->gen++;
    strncpy(s->key, key, sizeof s->key - 1); s->key[sizeof s->key - 1] = 0;
    s->state = P_WANT; s->use = ++g_ptex_clock; s->frame = g_ptex_frame; s->pw = s->ph = 0;
    pthread_cond_signal(&g_ptex_cv);
    pthread_mutex_unlock(&g_ptex_mu);
    return 0;
}

/* pixel dims of a READY texture for key (0,0 if not ready). Does NOT trigger a fetch.
 * Used to size a logo/backdrop by its native aspect ratio. */
static void poster_wh(const char *key, int *w, int *h) {
    *w = 0; *h = 0;
    pthread_mutex_lock(&g_ptex_mu);
    for (int i = 0; i < PT_CAP; i++)
        if (g_ptex[i].state == P_READY && strcmp(g_ptex[i].key, key) == 0) {
            *w = g_ptex[i].pw; *h = g_ptex[i].ph; break;
        }
    pthread_mutex_unlock(&g_ptex_mu);
}

/* MAIN/GL thread, once per frame BEFORE drawing. Uploads up to `budget` decoded slots. */
static void poster_pump(int budget) {
    pthread_mutex_lock(&g_ptex_mu);
    g_ptex_frame++;                       /* new frame: nothing "touched" yet */
    pthread_mutex_unlock(&g_ptex_mu);

    for (int done = 0; done < budget; done++) {
        pthread_mutex_lock(&g_ptex_mu);
        int idx = -1;
        for (int i = 0; i < PT_CAP; i++)
            if (g_ptex[i].state == P_DECODED) { idx = i; break; }
        if (idx < 0) { pthread_mutex_unlock(&g_ptex_mu); break; }
        pslot *s = &g_ptex[idx];
        unsigned char *px = s->px; int w = s->pw, h = s->ph; unsigned gen = s->gen;
        s->px = NULL; s->state = P_UPLOADING;
        pthread_mutex_unlock(&g_ptex_mu);

        GLuint t = img_upload_rgba(px, w, h);   /* GL, off the lock */
        img_free(px);

        pthread_mutex_lock(&g_ptex_mu);
        if (s->gen == gen && s->state == P_UPLOADING) {
            s->tex = t; s->state = t ? P_READY : P_FAILED;
        } else if (t) {
            glDeleteTextures(1, &t);            /* slot recycled mid-upload: drop */
        }
        pthread_mutex_unlock(&g_ptex_mu);
    }
}

/* BACKGROUND worker: claim a P_WANT slot, fetch+decode off-lock, publish P_DECODED. */
static void *poster_worker(void *arg) {
    (void)arg;
    for (;;) {
        pthread_mutex_lock(&g_ptex_mu);
        int idx = -1;
        while (!g_poster_quit) {
            for (int i = 0; i < PT_CAP; i++)
                if (g_ptex[i].state == P_WANT) { idx = i; break; }
            if (idx >= 0) break;
            struct timespec ts; clock_gettime(CLOCK_REALTIME, &ts);
            ts.tv_nsec += 50 * 1000000L;
            if (ts.tv_nsec >= 1000000000L) { ts.tv_sec++; ts.tv_nsec -= 1000000000L; }
            pthread_cond_timedwait(&g_ptex_cv, &g_ptex_mu, &ts);
        }
        if (g_poster_quit) { pthread_mutex_unlock(&g_ptex_mu); break; }
        pslot *s = &g_ptex[idx];
        char key[256]; strncpy(key, s->key, sizeof key); key[sizeof key - 1] = 0;
        unsigned gen = s->gen;
        s->state = P_LOADING;
        pthread_mutex_unlock(&g_ptex_mu);

        /* fetch (own-stack socket) + decode — NO gl* calls here */
        unsigned char *px = NULL; int w = 0, h = 0;
        http_stream hs;
        if (http_open(&hs, g_pms_host, g_pms_port, key, NULL) == 0) {
            int cap = (hs.content_length > 0 && hs.content_length < (32 << 20))
                      ? (int)hs.content_length + 16 : 65536;
            unsigned char *body = (unsigned char *)malloc(cap);
            int total = 0;
            if (body) {
                for (;;) {
                    if (cap - total < 4096) {              /* grow-by-doubling to EOF */
                        int nc = cap * 2;
                        unsigned char *nb = (unsigned char *)realloc(body, nc);
                        if (!nb) break;
                        body = nb; cap = nc;
                    }
                    int r = http_read(&hs, body + total, cap - total);
                    if (r <= 0) break;
                    total += r;
                }
                if (total > 0) px = img_decode_rgba(body, total, &w, &h);
                free(body);
            }
            http_close(&hs);
        }

        pthread_mutex_lock(&g_ptex_mu);
        if (s->gen == gen && s->state == P_LOADING) {
            if (px) { s->px = px; s->pw = w; s->ph = h; s->state = P_DECODED; }
            else s->state = P_FAILED;
        } else if (px) {
            img_free(px);                          /* recycled while we worked: discard */
        }
        pthread_mutex_unlock(&g_ptex_mu);
    }
    return NULL;
}

static void posters_init(const char *host, int port, const char *token) {
    g_pms_host = host; g_pms_port = port; g_pms_token = token;
    memset(g_ptex, 0, sizeof g_ptex);   /* state = P_EMPTY = 0 */
    g_poster_quit = 0; g_pworkers_n = 0;
    for (int i = 0; i < 2; i++)
        if (pthread_create(&g_pworkers[i], NULL, poster_worker, NULL) == 0) g_pworkers_n++;
}

static void posters_shutdown(void) {
    pthread_mutex_lock(&g_ptex_mu);
    g_poster_quit = 1;
    pthread_cond_broadcast(&g_ptex_cv);
    pthread_mutex_unlock(&g_ptex_mu);
    for (int i = 0; i < g_pworkers_n; i++) pthread_join(g_pworkers[i], NULL);
    for (int i = 0; i < PT_CAP; i++) {
        if (g_ptex[i].tex) { glDeleteTextures(1, &g_ptex[i].tex); g_ptex[i].tex = 0; }
        if (g_ptex[i].px)  { img_free(g_ptex[i].px); g_ptex[i].px = NULL; }
    }
}

#endif /* PLEXPOC_POSTERS_H */
