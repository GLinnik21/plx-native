#ifndef PLEXPOC_PMS_H
#define PLEXPOC_PMS_H
/* Plex Media Server library fetch + parse. Pulls /library/sections/<id>/all as
 * JSON over HTTP (stream.h) and scrapes each movie into pms_movies[]. Also hosts
 * urlenc (a generic Plex-URL percent-encoder shared by posters.c and playback.c).
 * Implementation in pms.c. */
#include <stddef.h>   /* size_t (urlenc proto) */

typedef struct {
    char       title[128];
    int        year;
    char       rating[12];      /* contentRating, e.g. "PG-13" */
    long long  dur_ns;          /* runtime */
    char       part[256];       /* /library/parts/<id>/<n>/file.<ext> (no host/token) */
    char       thumb[128];      /* /library/metadata/<id>/thumb/<t> (poster key) */
    char       art[128];        /* /library/metadata/<id>/art/<t> (backdrop key) */
    char       summary[600];
    char       rk[16];          /* ratingKey → /library/metadata/<rk> (transcode path) */
    char       vcodec[12];      /* Media[0] videoCodec (h264/hevc/av1…) */
    char       acodec[12];      /* Media[0] audioCodec (ac3/eac3/aac…) */
    float      blur[4][3];      /* UltraBlurColors: TL,TR,BR,BL corner RGB (0..1) */
    int        has_blur;
} pms_movie;

#define PMS_MAX_MOVIES 256
extern pms_movie pms_movies[PMS_MAX_MOVIES];
extern int       pms_nmovies;

/* Fetch section <sec> ("Movies" is 1) and parse into pms_movies[]. Returns count. */
int  pms_fetch_movies(const char *host, int port, const char *token, int sec);
/* percent-encode a Plex server-relative path for a transcode url= query value */
void urlenc(char *dst, size_t cap, const char *src);

#endif /* PLEXPOC_PMS_H */
