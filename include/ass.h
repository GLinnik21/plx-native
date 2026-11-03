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
 * (ass_set_fonts) only on a context's first load.
 * ABI 4: the UI text surface, plx_ass_text_* (src/ass_text.c). */
#define PLX_ASS_ABI_VERSION 4

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

/* ---- UI text (ABI 4) ------------------------------------------------------------------------
 * The application draws its own text with the television's SDL2_ttf 2.0.x: one face per call, a
 * left-to-right advance loop keyed on codepoints, no shaper. Arabic needs contextual joining and
 * Hebrew and Arabic both need right-to-left order, so the runs of those scripts are shaped here,
 * with the same pinned FriBidi, HarfBuzz and FreeType that already shape the subtitles. Every
 * other run stays on SDL2_ttf. One thread (the application's main thread) owns every handle. */
typedef struct PlxText PlxText;

typedef struct {
    int width;            /* box width in px: the advance, grown by any ink overhanging either end */
    int ascent;           /* rows from the box's top to the baseline: the ink's height above it */
    int height;           /* rows in the box: exactly the ink's, so 0 for a run of spaces */
    const uint8_t *alpha; /* width*height coverage, row-major, tightly packed. NULL from
                           * plx_ass_text_measure; from plx_ass_text_render, valid until the next
                           * call on the same handle or its close */
} PlxTextRun;

/* A face at `px` pixels per em (SDL_ttf's point size, which is pixels at its fixed 72 dpi).
 * NULL if the file will not open or `px` is outside 1..512. */
PlxText *plx_ass_text_open(const char *path, int px);
void plx_ass_text_close(PlxText *text);
/* Shape `len` bytes of UTF-8, one directional run in LOGICAL order (rtl: 1 right-to-left, 0
 * left-to-right), and report its box. measure and render lay the run out through the same code,
 * so a render's width is exactly its measure's. -1 if refused (bad arguments, a run over 4096
 * bytes, a box over the pixel cap), 0 on success. */
int plx_ass_text_measure(PlxText *text, const char *utf8, size_t len, int rtl, PlxTextRun *out);
int plx_ass_text_render(PlxText *text, const char *utf8, size_t len, int rtl, PlxTextRun *out);
/* The Unicode Bidirectional Algorithm (FriBidi) over ONE line of `n` codepoints, its paragraph
 * direction taken from the first strong character (left-to-right when there is none). Fills
 * `levels` with each character's resolved level after rule L1, `visual` with the logical index
 * drawn at each visual position (rule L2, left to right, combining marks NOT moved after their
 * base), and `mirrored` with each character's Bidi_Mirroring_Glyph where its level is odd and it
 * has one, else the character itself. Returns 1 if any level is odd, 0 if none, -1 if refused
 * (n outside 1..16384, a NULL array, or FriBidi failing). */
int plx_ass_text_bidi(const uint32_t *chars, int n, uint8_t *levels, int32_t *visual,
                      uint32_t *mirrored);

#endif
