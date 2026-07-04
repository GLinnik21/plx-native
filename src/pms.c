/* pms.c — Plex library fetch/scrape → pms_movies[]; plus urlenc (shared). */
#include <string.h>
#include <stdlib.h>
#include <stdio.h>
#include "stream.h"
#include "pms.h"

pms_movie pms_movies[PMS_MAX_MOVIES];
int       pms_nmovies = 0;

/* percent-encode a Plex server-relative path for the transcode url= query value */
void urlenc(char *dst, size_t cap, const char *src) {
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

/* scrape "key":"value" (a JSON string) within [s,end); 1 + fills out on success */
static int pms_scrape_str(const char *s, const char *end, const char *key, char *out, int osz) {
    char pat[48]; snprintf(pat, sizeof pat, "\"%s\":\"", key);
    const char *p = strstr(s, pat);
    if (!p || p >= end) { out[0] = 0; return 0; }
    p += strlen(pat);
    int i = 0;
    while (p < end && *p != '"' && i < osz - 1) {
        if (*p == '\\' && p + 1 < end) { p++; if (*p == 'n') { out[i++] = ' '; p++; continue; } }
        out[i++] = *p++;
    }
    out[i] = 0;
    return 1;
}
/* scrape "key":<number> within [s,end); -1 if absent */
static long long pms_scrape_num(const char *s, const char *end, const char *key) {
    char pat[48]; snprintf(pat, sizeof pat, "\"%s\":", key);
    const char *p = strstr(s, pat);
    if (!p || p >= end) return -1;
    return strtoll(p + strlen(pat), NULL, 10);
}
/* parse a "rrggbb" hex color into RGB floats (0..1) */
static void pms_hex3(const char *hex, float out[3]) {
    unsigned v = (unsigned)strtoul(hex, NULL, 16);
    out[0] = (float)((v >> 16) & 0xff) / 255.0f;
    out[1] = (float)((v >> 8) & 0xff) / 255.0f;
    out[2] = (float)(v & 0xff) / 255.0f;
}

/* Fetch section <sec> ("Movies" is 1) and parse into pms_movies[]. Returns count. */
int pms_fetch_movies(const char *host, int port, const char *token, int sec) {
    http_stream hs;
    char path[160];
    snprintf(path, sizeof path, "/library/sections/%d/all?X-Plex-Token=%s", sec, token);
    if (http_open(&hs, host, port, path, "Accept: application/json\r\n") != 0) return 0;
    static char buf[1024 * 1024];
    int total = 0, r;
    while (total < (int)sizeof buf - 1 &&
           (r = http_read(&hs, (unsigned char *)buf + total, (int)sizeof buf - 1 - total)) > 0)
        total += r;
    http_close(&hs);
    buf[total] = 0;

    pms_nmovies = 0;
    /* Each movie is a Metadata array element; every element starts with
     * "ratingKey": (nested Media/Part/Genre objects never do), so we bound each
     * item as [ratingKey_i, ratingKey_{i+1}) and scrape its fields. */
    const char *p = strstr(buf, "\"Metadata\":");
    if (!p) return 0;
    while (pms_nmovies < PMS_MAX_MOVIES) {
        const char *item = strstr(p, "\"ratingKey\":");
        if (!item) break;
        const char *next = strstr(item + 12, "\"ratingKey\":");
        const char *end  = next ? next : buf + total;
        pms_movie *m = &pms_movies[pms_nmovies];
        memset(m, 0, sizeof *m);
        pms_scrape_str(item, end, "title", m->title, sizeof m->title);
        m->year = (int)pms_scrape_num(item, end, "year");
        pms_scrape_str(item, end, "contentRating", m->rating, sizeof m->rating);
        long long durms = pms_scrape_num(item, end, "duration");
        m->dur_ns = durms > 0 ? durms * 1000000LL : 0;
        pms_scrape_str(item, end, "thumb", m->thumb, sizeof m->thumb);
        pms_scrape_str(item, end, "art", m->art, sizeof m->art);
        pms_scrape_str(item, end, "summary", m->summary, sizeof m->summary);
        pms_scrape_str(item, end, "ratingKey", m->rk, sizeof m->rk);
        pms_scrape_str(item, end, "videoCodec", m->vcodec, sizeof m->vcodec);
        pms_scrape_str(item, end, "audioCodec", m->acodec, sizeof m->acodec);
        { char tl[8], tr[8], br[8], bl[8];    /* UltraBlurColors → ambient gradient */
          if (pms_scrape_str(item, end, "topLeft", tl, sizeof tl)) {
              pms_scrape_str(item, end, "topRight", tr, sizeof tr);
              pms_scrape_str(item, end, "bottomRight", br, sizeof br);
              pms_scrape_str(item, end, "bottomLeft", bl, sizeof bl);
              pms_hex3(tl, m->blur[0]); pms_hex3(tr, m->blur[1]);
              pms_hex3(br, m->blur[2]); pms_hex3(bl, m->blur[3]);
              m->has_blur = 1;
          } }
        const char *pk = strstr(item, "/library/parts/");   /* the Part.key */
        if (pk && pk < end) { int i = 0;
            while (pk < end && *pk != '"' && i < (int)sizeof m->part - 1) m->part[i++] = *pk++;
            m->part[i] = 0; }
        if (m->title[0] && m->part[0]) pms_nmovies++;
        if (!next) break;
        p = next;
    }
    return pms_nmovies;
}
