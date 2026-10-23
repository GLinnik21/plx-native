/* The UI text facade (ABI 4): Arabic and Hebrew runs for the application's own text renderer.
 *
 * The application draws its labels with the television's SDL2_ttf 2.0.x, which cannot shape or
 * reorder (include/ass.h says why that matters). This file gives it the two halves it lacks, from
 * the font stack this library already links for the subtitles:
 *
 *   plx_ass_text_bidi     FriBidi: resolved levels (rule L1 applied), the visual order (L2) and
 *                         mirrored brackets for one line. The caller cuts the line into runs.
 *   plx_ass_text_measure  HarfBuzz shapes one directional run of one face; FreeType rasterizes
 *   plx_ass_text_render   it into an 8-bit coverage box with its own baseline.
 *
 * Positions come from HarfBuzz's own OpenType functions at the requested pixel size, unhinted and
 * exact, so the joins of a cursive script meet where the font draws them at every size. Each glyph
 * is then loaded with FreeType's light hinting (vertical snapping only, the contract every SDL2_ttf
 * face in the app is opened under) and rendered at its sub-pixel x; y offsets (mark positions) are
 * rounded to whole pixels so hinted horizontal strokes stay sharp. Overlapping coverage, which a
 * cursive join and a mark over its base both produce, combines by maximum, not by sum.
 *
 * Measure and render are ONE layout pass: every glyph is rendered into its own bitmap, the box is
 * the union of those bitmaps and the pen's advance, and render then blits the same bitmaps. A
 * render's width is therefore its measure's by construction, which is what lets the application
 * lay a label out from measured widths and draw it later without drift.
 *
 * One thread owns every handle (the application's main thread): the FreeType library and the
 * HarfBuzz faces below are this file's own statics, separate from libass's on the subtitle worker.
 */
#include "../include/ass.h"

#include <ft2build.h>
#include FT_FREETYPE_H
#include FT_GLYPH_H
#include <fribidi/fribidi.h>
#include <harfbuzz/hb.h>

#include <limits.h>
#include <stdlib.h>
#include <string.h>

#define TEXT_MIN_PX 1
#define TEXT_MAX_PX 512
#define TEXT_MAX_RUN_BYTES 4096u
#define TEXT_MAX_PIXELS (4096u * 1024u)
#define TEXT_MAX_BIDI_CHARS 16384
/* Distinct font FILES whose HarfBuzz face is kept for the process (the application ships four). */
#define TEXT_FACE_FILES 8

struct PlxText {
    FT_Face ft;
    hb_font_t *hb;
    uint8_t *pixels;
    size_t capacity;
};

struct Placed {
    FT_BitmapGlyph bitmap; /* NULL for a glyph with no image (a space, a missing glyph) */
    long left;             /* box column of the bitmap's first column, before the origin shift */
    long top;              /* pixels from the baseline UP to the bitmap's first row */
};

static FT_Library library;
static struct {
    char *path;
    hb_face_t *face;
} face_files[TEXT_FACE_FILES];

static long floor64(FT_Pos v) { return v >= 0 ? (long)(v / 64) : -(long)((-v + 63) / 64); }
static long ceil64(FT_Pos v) { return -floor64(-v); }

/* One HarfBuzz face per font file, shared by every size of it and kept for the process. */
static hb_face_t *shared_face(const char *path)
{
    for (int i = 0; i < TEXT_FACE_FILES; i++)
        if (face_files[i].path && strcmp(face_files[i].path, path) == 0)
            return face_files[i].face;
    for (int i = 0; i < TEXT_FACE_FILES; i++) {
        if (face_files[i].path)
            continue;
        hb_blob_t *blob = hb_blob_create_from_file_or_fail(path);
        if (!blob)
            return NULL;
        hb_face_t *face = hb_face_create(blob, 0);
        hb_blob_destroy(blob);
        char *copy = strdup(path);
        if (!copy || hb_face_get_glyph_count(face) == 0) {
            free(copy);
            hb_face_destroy(face);
            return NULL;
        }
        face_files[i].path = copy;
        face_files[i].face = face;
        return face;
    }
    return NULL;
}

void plx_ass_text_close(PlxText *text)
{
    if (!text)
        return;
    hb_font_destroy(text->hb);
    if (text->ft)
        FT_Done_Face(text->ft);
    free(text->pixels);
    free(text);
}

PlxText *plx_ass_text_open(const char *path, int px)
{
    if (!path || px < TEXT_MIN_PX || px > TEXT_MAX_PX)
        return NULL;
    if (!library && FT_Init_FreeType(&library) != 0) {
        library = NULL;
        return NULL;
    }
    hb_face_t *face = shared_face(path);
    if (!face)
        return NULL;
    PlxText *text = calloc(1, sizeof *text);
    if (!text)
        return NULL;
    /* SDL2_ttf opens at FT_Set_Char_Size(ptsize * 64) and 72 dpi, so its point size IS pixels per
     * em; the same here keeps a run's em equal to the Inter beside it. */
    if (FT_New_Face(library, path, 0, &text->ft) != 0) {
        text->ft = NULL;
        plx_ass_text_close(text);
        return NULL;
    }
    if (FT_Set_Char_Size(text->ft, 0, (FT_F26Dot6)px * 64, 72, 72) != 0) {
        plx_ass_text_close(text);
        return NULL;
    }
    text->hb = hb_font_create(face);
    hb_font_set_scale(text->hb, px * 64, px * 64); /* positions in 26.6 pixels */
    return text;
}

static void release(struct Placed *glyphs, unsigned count)
{
    for (unsigned i = 0; glyphs && i < count; i++)
        if (glyphs[i].bitmap)
            FT_Done_Glyph((FT_Glyph)glyphs[i].bitmap);
    free(glyphs);
}

static int layout(PlxText *text, const char *utf8, size_t len, int rtl, PlxTextRun *out, int render)
{
    if (!text || !out || (len && !utf8) || len > TEXT_MAX_RUN_BYTES)
        return -1;
    memset(out, 0, sizeof *out);
    hb_buffer_t *buf = hb_buffer_create();
    if (!hb_buffer_allocation_successful(buf)) {
        hb_buffer_destroy(buf);
        return -1;
    }
    hb_buffer_add_utf8(buf, len ? utf8 : "", (int)len, 0, (int)len);
    hb_buffer_set_direction(buf, rtl ? HB_DIRECTION_RTL : HB_DIRECTION_LTR);
    hb_buffer_guess_segment_properties(buf);
    hb_shape(text->hb, buf, NULL, 0);
    unsigned count = 0;
    const hb_glyph_info_t *info = hb_buffer_get_glyph_infos(buf, &count);
    const hb_glyph_position_t *pos = hb_buffer_get_glyph_positions(buf, NULL);
    struct Placed *glyphs = count ? calloc(count, sizeof *glyphs) : NULL;
    if (count && !glyphs) {
        hb_buffer_destroy(buf);
        return -1;
    }

    /* Glyphs arrive in VISUAL order (HarfBuzz reverses a right-to-left run itself), each advance
     * positive, so the pen walks left to right from the run's origin in both directions.
     *
     * The box is the INK, not the face's line box: Noto Kufi's descender is 0.615 em against
     * Inter's 0.241, so a face box would make every Arabic label measure taller than its Latin
     * neighbours whether or not a letter descends. The caller keeps its own line box and grows it
     * only by what is actually drawn. */
    long min_x = 0, max_x = 0, ascent = 0, below = 0;
    FT_Pos pen_x = 0, pen_y = 0;
    for (unsigned i = 0; i < count; i++) {
        FT_Pos gx = pen_x + pos[i].x_offset, gy = pen_y + pos[i].y_offset;
        pen_x += pos[i].x_advance;
        pen_y += pos[i].y_advance;
        if (FT_Load_Glyph(text->ft, info[i].codepoint,
                          FT_LOAD_DEFAULT | FT_LOAD_TARGET_LIGHT | FT_LOAD_NO_BITMAP) != 0)
            continue;
        FT_Glyph glyph;
        if (FT_Get_Glyph(text->ft->glyph, &glyph) != 0)
            continue;
        FT_Vector shift = {gx - floor64(gx) * 64, 0}; /* the sub-pixel part of x */
        if (FT_Glyph_To_Bitmap(&glyph, FT_RENDER_MODE_NORMAL, &shift, 1) != 0) {
            FT_Done_Glyph(glyph);
            continue;
        }
        FT_BitmapGlyph bitmap = (FT_BitmapGlyph)glyph;
        if (bitmap->bitmap.pixel_mode != FT_PIXEL_MODE_GRAY || bitmap->bitmap.width == 0 ||
            bitmap->bitmap.rows == 0) {
            FT_Done_Glyph(glyph);
            continue;
        }
        glyphs[i].bitmap = bitmap;
        glyphs[i].left = floor64(gx) + bitmap->left;
        glyphs[i].top = floor64(gy + 32) + bitmap->top;
        long right = glyphs[i].left + (long)bitmap->bitmap.width;
        if (glyphs[i].left < min_x)
            min_x = glyphs[i].left;
        if (right > max_x)
            max_x = right;
        if (glyphs[i].top > ascent)
            ascent = glyphs[i].top;
        if ((long)bitmap->bitmap.rows - glyphs[i].top > below)
            below = (long)bitmap->bitmap.rows - glyphs[i].top;
    }
    hb_buffer_destroy(buf);
    if (ceil64(pen_x) > max_x)
        max_x = ceil64(pen_x);
    long width = max_x - min_x, height = ascent + below;
    if (width < 0 || height < 0 || width > INT_MAX || height > INT_MAX ||
        (unsigned long)width * (unsigned long)height > TEXT_MAX_PIXELS) {
        release(glyphs, count);
        return -1;
    }
    out->width = (int)width;
    out->ascent = (int)ascent;
    out->height = (int)height;
    if (!render) {
        release(glyphs, count);
        return 0;
    }

    size_t bytes = (size_t)width * (size_t)height;
    if (bytes > text->capacity) {
        uint8_t *grown = realloc(text->pixels, bytes);
        if (!grown) {
            release(glyphs, count);
            return -1;
        }
        text->pixels = grown;
        text->capacity = bytes;
    }
    if (bytes)
        memset(text->pixels, 0, bytes);
    for (unsigned i = 0; i < count; i++) {
        const FT_BitmapGlyph g = glyphs[i].bitmap;
        if (!g)
            continue;
        long x0 = glyphs[i].left - min_x, y0 = ascent - glyphs[i].top;
        for (unsigned row = 0; row < g->bitmap.rows; row++) {
            long y = y0 + (long)row;
            if (y < 0 || y >= height)
                continue;
            const uint8_t *src = g->bitmap.buffer + (ptrdiff_t)row * g->bitmap.pitch;
            uint8_t *dst = text->pixels + (size_t)y * (size_t)width;
            for (unsigned col = 0; col < g->bitmap.width; col++) {
                long x = x0 + (long)col;
                if (x >= 0 && x < width && src[col] > dst[x])
                    dst[x] = src[col];
            }
        }
    }
    release(glyphs, count);
    out->alpha = bytes ? text->pixels : NULL;
    return 0;
}

int plx_ass_text_measure(PlxText *text, const char *utf8, size_t len, int rtl, PlxTextRun *out)
{
    return layout(text, utf8, len, rtl, out, 0);
}

int plx_ass_text_render(PlxText *text, const char *utf8, size_t len, int rtl, PlxTextRun *out)
{
    return layout(text, utf8, len, rtl, out, 1);
}

int plx_ass_text_bidi(const uint32_t *chars, int n, uint8_t *levels, int32_t *visual,
                      uint32_t *mirrored)
{
    if (!chars || !levels || !visual || !mirrored || n < 1 || n > TEXT_MAX_BIDI_CHARS)
        return -1;
    FriBidiCharType *types = malloc((size_t)n * sizeof *types);
    FriBidiBracketType *brackets = malloc((size_t)n * sizeof *brackets);
    FriBidiLevel *resolved = malloc((size_t)n * sizeof *resolved);
    FriBidiStrIndex *map = malloc((size_t)n * sizeof *map);
    int result = -1;
    if (types && brackets && resolved && map) {
        fribidi_get_bidi_types(chars, n, types);
        fribidi_get_bracket_types(chars, n, types, brackets);
        FriBidiParType direction = FRIBIDI_PAR_ON; /* rules P2/P3: the first strong character */
        if (fribidi_get_par_embedding_levels_ex(types, brackets, n, &direction, resolved)) {
            memcpy(mirrored, chars, (size_t)n * sizeof *mirrored);
            fribidi_shape_mirroring(resolved, n, mirrored);
            for (int i = 0; i < n; i++)
                map[i] = i;
            /* Flags 0: no FRIBIDI_FLAG_REORDER_NSM, so a right-to-left run stays one contiguous
             * logical range in visual order; HarfBuzz places its marks itself. */
            if (fribidi_reorder_line(0, types, n, 0, direction, resolved, NULL, map)) {
                result = 0;
                for (int i = 0; i < n; i++) {
                    levels[i] = (uint8_t)resolved[i];
                    visual[i] = (int32_t)map[i];
                    if (resolved[i] & 1)
                        result = 1;
                }
            }
        }
    }
    free(types);
    free(brackets);
    free(resolved);
    free(map);
    return result;
}
