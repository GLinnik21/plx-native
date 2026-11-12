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
    /* PREMULTIPLIED RGBA (each colour channel at most its alpha; a pixel of coverage 50% of
     * colour (255, 128, 64) is 128, 64, 32, 128), composed in libass image-list order; valid until
     * the next render or destroy. The caller draws it with a premultiplied blend (ONE,
     * ONE_MINUS_SRC_ALPHA); drawn with a straight one it would be dark at every soft edge. */
    const uint8_t *rgba;
} PlxAssBitmap;

#define PLX_ASS_MAX_REGIONS 64
typedef struct {
    size_t count;
    const PlxAssBitmap *regions; /* disjoint bounds; same lifetime as their pixels */
} PlxAssFrame;

/* ABI 3: plx_ass_set_features, plx_ass_set_cache_limits, plx_ass_set_line_spacing,
 * plx_ass_replace_track and plx_ass_stats join the 2 surface. plx_ass_load now scans the fonts
 * (ass_set_fonts) only on a context's first load.
 * ABI 4: the UI text surface, plx_ass_text_* (src/ass_text.c).
 * ABI 5: plx_ass_add_font works after plx_ass_load too (the face is opened alone at the next
 * frame) and has no byte limit but `int`; before, it refused once a track was loaded and past
 * 64 MiB in all. plx_ass_add_font_borrowed adds a font the caller keeps, without copying it.
 * PlxAssStats also carries the last plx_ass_render call's timing (wall and thread
 * CPU) and image counts (last_*), the split a styled-subtitle frame's cost is attributed with.
 * plx_ass_render publishes PREMULTIPLIED RGBA, where it published straight RGBA before: the straight
 * conversion was a pass over every pixel of every changed region (mostly transparent ones) and the
 * application's blend takes premultiplied colour as it is. The version stays 5 because ABI 5 has not
 * shipped (main is on 4) and the one consumer, plx_media's `player::ass`, builds with this library
 * from the same tree. */
#define PLX_ASS_ABI_VERSION 5

/* The last_* fields describe the most recent plx_ass_render call, in microseconds saturating at
 * UINT32_MAX. last_libass_us is the wall time (CLOCK_MONOTONIC) of ass_render_frame alone;
 * last_composite_us is the wall time of everything after it until the frame is returned (bounds,
 * region placement and the per-pixel composite), 0 for a call that found
 * the frame unchanged (it returns 0). last_libass_cpu_us and last_composite_cpu_us are the same
 * two phases measured on CLOCK_THREAD_CPUTIME_ID: the CPU time the calling thread spent in them,
 * so wall minus CPU is the time it was descheduled or blocked (the host's other work, a page
 * fault) rather than computing. A clock that cannot be read counts 0. last_images and
 * last_image_pixels (the sum of w*h) count the ASS_Images of the last frame that was composed, so
 * an unchanged call leaves them as they were. All of them are 0 before the first render. Eight
 * 32-bit fields come before the 64-bit one, so last_image_pixels sits on its natural 8-byte
 * boundary (offset 32, size 40) on the ARM target and the host alike. */
typedef struct {
    uint32_t library_fonts; /* fonts the library holds: added through plx_ass_add_font or plx_ass_add_font_borrowed, or extracted from [Fonts] */
    uint32_t font_scans;    /* times the renderer opened every font (ass_set_fonts) */
    uint32_t tracks;        /* tracks loaded or replaced */
    uint32_t last_libass_us;
    uint32_t last_composite_us;
    uint32_t last_libass_cpu_us;
    uint32_t last_composite_cpu_us;
    uint32_t last_images;
    uint64_t last_image_pixels;
} PlxAssStats;

unsigned plx_ass_abi_version(void);
PlxAss *plx_ass_create(const char *default_font);
void plx_ass_destroy(PlxAss *ctx);
/* Copy one font file into the library under `name`. -1 for a NULL or empty argument or a `len`
 * past INT_MAX (libass takes an int), else 0. Valid before and after plx_ass_load: a font added
 * after it is opened by libass alone, at the start of the next plx_ass_render that has events, and
 * the faces already open are not scanned again (PlxAssStats.font_scans stays 1). A face that
 * FreeType cannot open is skipped silently by libass; the copy is still held. */
int plx_ass_add_font(PlxAss *ctx, const char *name, const uint8_t *data, size_t len);
/* plx_ass_add_font without the copy: the library holds `data` itself, not a copy of it, so one
 * buffer can back the same font in every context that is handed it. Same arguments, same -1s, same
 * timing of the face scan. `data` must stay valid and unchanged until plx_ass_destroy of THIS context
 * has returned (the renderer reads it for as long as it lives); destroy never frees it. */
int plx_ass_add_font_borrowed(PlxAss *ctx, const char *name, const uint8_t *data, size_t len);
int plx_ass_load(PlxAss *ctx, const uint8_t *data, size_t len, int script);
/* Replace the loaded track on the same library and renderer, keeping every font face the
 * renderer has opened. The new track's own [Fonts] section is NOT extracted, so it must repeat the
 * first track's (a synthesized header has none; a window of the same script repeats the same
 * section); fonts the caller adds with plx_ass_add_font are unaffected. Needs a first plx_ass_load. The next render returns a
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
