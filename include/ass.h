#ifndef PLX_ASS_H
#define PLX_ASS_H

/* Private, versioned facade over the pinned bundled libass. No upstream struct
 * crosses this ABI; every handle and returned pixel buffer belongs to one worker. */
#include <stddef.h>
#include <stdint.h>

typedef struct PlxAss PlxAss;
typedef struct {
    int x, y, width, height;
    size_t bytes;
    const uint8_t *rgba; /* straight RGBA; valid until the next render or destroy */
} PlxAssBitmap;

#define PLX_ASS_MAX_REGIONS 64
typedef struct {
    size_t count;
    const PlxAssBitmap *regions; /* disjoint bounds; same lifetime as their pixels */
} PlxAssFrame;

/* ABI 3: plx_ass_set_features, plx_ass_set_cache_limits, plx_ass_set_line_spacing,
 * plx_ass_replace_track and plx_ass_stats join the 2 surface. plx_ass_load now scans the fonts
 * (ass_set_fonts) only on a context's first load. */
#define PLX_ASS_ABI_VERSION 3

typedef struct {
    uint32_t library_fonts; /* fonts the library holds: added through plx_ass_add_font or extracted */
    uint32_t font_scans;    /* times the renderer opened every font (ass_set_fonts) */
    uint32_t tracks;        /* tracks loaded or replaced */
} PlxAssStats;

unsigned plx_ass_abi_version(void);
PlxAss *plx_ass_create(const char *default_font);
void plx_ass_destroy(PlxAss *ctx);
int plx_ass_add_font(PlxAss *ctx, const char *name, const uint8_t *data, size_t len);
int plx_ass_load(PlxAss *ctx, const uint8_t *data, size_t len, int script);
/* Replace the loaded track on the same library and renderer, keeping every font face the
 * renderer has opened. Fonts cannot change here: the new track's own [Fonts] section is NOT
 * extracted, so it must repeat the first track's (a synthesized header has none; a window of the
 * same script repeats the same section). Needs a first plx_ass_load. The next render returns a
 * full frame, never 0 "unchanged". -1 leaves the context without a track; such a context takes only
 * another plx_ass_replace_track (plx_ass_load refuses once the fonts are scanned). */
int plx_ass_replace_track(PlxAss *ctx, const uint8_t *data, size_t len, int script);
/* Track features the facade re-applies on every load and replace. wrap_unicode: break lines by
 * the Unicode line-breaking rules (ASS_FEATURE_WRAP_UNICODE) rather than at spaces only.
 * -1 if the library cannot (built without libunibreak) or the context is unusable. */
int plx_ass_set_features(PlxAss *ctx, int wrap_unicode);
/* libass glyph (count) and bitmap (MiB) cache bounds. -1 for a value below 1. */
int plx_ass_set_cache_limits(PlxAss *ctx, int glyph_max, int bitmap_mib);
/* Extra pixels between lines at the render size; negative tightens. -1 if not finite or beyond
 * +-1000. The next render redraws in full. */
int plx_ass_set_line_spacing(PlxAss *ctx, double spacing);
int plx_ass_stats(const PlxAss *ctx, PlxAssStats *out);
int plx_ass_chunk(PlxAss *ctx, const uint8_t *data, size_t len,
                  int64_t start_ms, int64_t duration_ms);
void plx_ass_prune_before(PlxAss *ctx, int64_t deadline_ms);
void plx_ass_flush_events(PlxAss *ctx);
/* -1: refused/failed, 0: identical output (frame unchanged), 1: changed output.
 * A changed frame with count=0 clears the old subtitle. */
int plx_ass_render(PlxAss *ctx, int64_t now_ms, int width, int height,
                   int storage_width, int storage_height,
                   PlxAssFrame *frame);

#endif
