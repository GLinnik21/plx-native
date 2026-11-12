/* Compiled INTO libass-plx, never linked into the app. The implementation reads
 * ASS_Image/ASS_Track through the exact pinned libass headers used to build it. */
#include "../include/ass.h"
#include "ass_composite.h"
#include "ass_internal.h"
#include <ass/ass.h>
#include <limits.h>
#include <math.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define MAX_SCRIPT_BYTES (8u * 1024u * 1024u)
#define MAX_EVENT_BYTES (256u * 1024u)
#define MAX_EVENTS 20000
#define MAX_PIXELS (3840u * 2160u)
#define MAX_IMAGES 16384
#define MAX_LINE_SPACING 1000.0
/* libass bounds its glyph/bitmap caches, in glyphs / MiB respectively. */
#define DEFAULT_GLYPH_CACHE 1000
#define DEFAULT_BITMAP_CACHE_MIB 24

struct ImageKey {
    uintptr_t bitmap;
    uint32_t color;
    size_t region;
    int x, y, width, height, stride;
};

struct PlxAss {
    ASS_Library *library;
    ASS_Renderer *renderer;
    ASS_Track *track;
    uint8_t *rgba;
    size_t bytes;
    PlxAssBitmap regions[PLX_ASS_MAX_REGIONS];
    struct ImageKey *images;
    size_t image_capacity, image_count, region_count;
    int cache_valid;
    int width, height, storage_width, storage_height;
    int first;
    int fonts_set;      /* ass_set_fonts has run: it opens every font, so only once per ctx */
    int wrap_unicode;
    uint32_t font_scans, tracks;
    /* The last plx_ass_render call's cost split, reported by plx_ass_stats. */
    uint32_t last_libass_us, last_composite_us, last_libass_cpu_us, last_composite_cpu_us;
    uint32_t last_images;
    uint64_t last_image_pixels;
    char *default_font;
    /* Indices (into the library's font table) of the fonts added by plx_ass_add_font_borrowed: their
     * bytes are the caller's, so plx_ass_destroy detaches them before the library frees its own. */
    size_t *borrowed;
    size_t borrowed_count, borrowed_capacity;
};

/* Subtitle text and font metadata are untrusted and can be private. Do not
 * forward libass's per-event diagnostic output to the application's event log. */
static void quiet_message(int level, const char *format, va_list args, void *data)
{
    (void)level; (void)format; (void)args; (void)data;
}

unsigned plx_ass_abi_version(void) { return PLX_ASS_ABI_VERSION; }

/* A stopwatch on one clock: CLOCK_MONOTONIC for wall time, CLOCK_THREAD_CPUTIME_ID for the CPU time
 * the calling thread has run. A clock that cannot be read stops the watch: every lap after the
 * failure counts 0, never a span measured from a start that was not taken. */
struct Lap {
    clockid_t clock;
    struct timespec at;
    int running;
};

static void lap_start(struct Lap *lap, clockid_t clock)
{
    lap->clock = clock;
    lap->running = clock_gettime(clock, &lap->at) == 0;
}

/* Microseconds since the last lap (or the start), which becomes now: one clock read ends a phase
 * and starts the next. */
static uint32_t lap_us(struct Lap *lap)
{
    struct timespec now;
    if (!lap->running) return 0;
    if (clock_gettime(lap->clock, &now) != 0) {
        lap->running = 0;
        return 0;
    }
    int64_t ns = ((int64_t)now.tv_sec - (int64_t)lap->at.tv_sec) * 1000000000 +
                 ((int64_t)now.tv_nsec - (int64_t)lap->at.tv_nsec);
    lap->at = now;
    if (ns <= 0) return 0;
    return ns / 1000 >= UINT32_MAX ? UINT32_MAX : (uint32_t)(ns / 1000);
}

void plx_ass_destroy(PlxAss *ctx)
{
    if (!ctx) return;
    if (ctx->track) ass_free_track(ctx->track);
    if (ctx->renderer) ass_renderer_done(ctx->renderer);
    /* The renderer's faces read a borrowed font until it is done; the library frees every font's
     * bytes, so those that are the caller's leave its table first. */
    plxi_library_forget_borrowed(ctx->library, ctx->borrowed, ctx->borrowed_count);
    if (ctx->library) ass_library_done(ctx->library);
    free(ctx->borrowed);
    free(ctx->rgba);
    free(ctx->images);
    free(ctx->default_font);
    free(ctx);
}

PlxAss *plx_ass_create(const char *default_font)
{
    if (!default_font || !*default_font) return NULL;
    PlxAss *ctx = calloc(1, sizeof(*ctx));
    if (!ctx) return NULL;
    ctx->default_font = strdup(default_font);
    ctx->library = ass_library_init();
    if (!ctx->default_font || !ctx->library) goto failed;
    ass_set_message_cb(ctx->library, quiet_message, NULL);
    ass_set_extract_fonts(ctx->library, 1);
    ctx->renderer = ass_renderer_init(ctx->library);
    if (!ctx->renderer) goto failed;
    ass_set_cache_limits(ctx->renderer, DEFAULT_GLYPH_CACHE, DEFAULT_BITMAP_CACHE_MIB);
    ctx->first = 1;
    return ctx;
failed:
    plx_ass_destroy(ctx);
    return NULL;
}

/* Hand libass one font file. libass copies the bytes (ass_add_font) and opens the face at the next
 * ass_set_fonts or, once a track has events, at the start of the next frame
 * (ass_render.c: ass_start_frame -> ass_update_embedded_fonts, which scans only the fonts added
 * since), so this works before and after plx_ass_load and costs one face scan for this font, not a
 * rescan of the others. The only limit is the `int` size ass_add_font takes: what a file attaches
 * is never cut by a byte count (the Rust side chooses WHICH fonts to add, by name). */
int plx_ass_add_font(PlxAss *ctx, const char *name, const uint8_t *data, size_t len)
{
    if (!ctx || !name || !*name || !data || !len || len > INT_MAX)
        return -1;
    ass_add_font(ctx->library, name, (char *)data, (int)len);
    return 0;
}

/* plx_ass_add_font without the copy: libass's entry points at the caller's `data`, which the caller
 * keeps alive and unchanged until plx_ass_destroy returns. One font held by N renderers (the
 * subtitle worker pool) is then one buffer, not N (issue #1328). The face is opened the same way,
 * at the next frame. The destroy path detaches the entry so libass never frees the caller's bytes. */
int plx_ass_add_font_borrowed(PlxAss *ctx, const char *name, const uint8_t *data, size_t len)
{
    if (!ctx || !name || !*name || !data || !len || len > INT_MAX)
        return -1;
    if (ctx->borrowed_count == ctx->borrowed_capacity) {
        size_t capacity = ctx->borrowed_capacity ? 2 * ctx->borrowed_capacity : 16;
        size_t *grown = realloc(ctx->borrowed, capacity * sizeof(*grown));
        if (!grown) return -1;
        ctx->borrowed = grown;
        ctx->borrowed_capacity = capacity;
    }
    size_t index;
    if (plxi_library_borrow_font(ctx->library, name, data, len, &index) != 0)
        return -1;
    ctx->borrowed[ctx->borrowed_count++] = index;
    return 0;
}

/* The features of the loaded track, from the ctx. The track is replaced as a whole on every
 * load, and libass keeps them per track, so they are applied here each time. */
static int apply_features(PlxAss *ctx)
{
    if (!ctx->track) return 0;
    return ass_track_set_feature(ctx->track, ASS_FEATURE_WRAP_UNICODE, ctx->wrap_unicode);
}

/* Read `data` as the ctx's track. On failure the ctx holds no track. */
static int read_track(PlxAss *ctx, const uint8_t *data, size_t len, int script)
{
    if (!data || !len || len > MAX_SCRIPT_BYTES) return -1;
    if (script) {
        ctx->track = ass_read_memory(ctx->library, (char *)data, len, NULL);
    } else {
        ctx->track = ass_new_track(ctx->library);
        if (ctx->track) ass_process_codec_private(ctx->track, (char *)data, (int)len);
    }
    if (!ctx->track || ctx->track->track_type == TRACK_TYPE_UNKNOWN ||
        ctx->track->n_styles <= 0 || ctx->track->n_events > MAX_EVENTS ||
        (script && ctx->track->n_events == 0)) {
        if (ctx->track) ass_free_track(ctx->track);
        ctx->track = NULL;
        return -1;
    }
    /* Matroska ReadOrder is the deduplication key, including after demux replay. */
    ass_set_check_readorder(ctx->track, 1);
    /* Not supported without libunibreak: only an enabled flag can fail. */
    if (apply_features(ctx) < 0 && ctx->wrap_unicode) {
        ass_free_track(ctx->track);
        ctx->track = NULL;
        return -1;
    }
    ++ctx->tracks;
    return 0;
}

int plx_ass_load(PlxAss *ctx, const uint8_t *data, size_t len, int script)
{
    if (!ctx || ctx->track || ctx->fonts_set) return -1;
    if (read_track(ctx, data, len, script) < 0) return -1;
    /* Inter is the shipped Latin family. With provider NONE libass tries the
     * exact authored family, this family, then default_font (our CJK face).
     * ass_set_fonts opens every font the library holds, so it runs once per ctx, here: a second
     * load refuses, a replaced track reuses the faces, and a font added from now on (plx_ass_add_font)
     * is opened alone at the next frame. */
    ass_set_fonts(ctx->renderer, ctx->default_font, "Inter", ASS_FONTPROVIDER_NONE, NULL, 1);
    ctx->fonts_set = 1;
    ++ctx->font_scans;
    return 0;
}

int plx_ass_replace_track(PlxAss *ctx, const uint8_t *data, size_t len, int script)
{
    if (!ctx || !ctx->fonts_set) return -1;
    /* Extraction is off because a replacement track's [Fonts] section would be registered and
     * scanned at the next frame (ass_start_frame -> ass_update_embedded_fonts), duplicating faces
     * the caller already added with plx_ass_add_font. Which faces a renderer holds is the Rust
     * side's decision (font_names). */
    ass_set_extract_fonts(ctx->library, 0);
    if (ctx->track) ass_free_track(ctx->track);
    ctx->track = NULL;
    /* Per-track frame state, as a fresh ctx has it: otherwise libass's "unchanged" for the
     * new track's first frame would be answered with the old track's regions. */
    ctx->first = 1;
    ctx->cache_valid = 0;
    ctx->region_count = ctx->image_count = 0;
    return read_track(ctx, data, len, script);
}

int plx_ass_set_features(PlxAss *ctx, int wrap_unicode)
{
    if (!ctx) return -1;
    wrap_unicode = wrap_unicode != 0;
    if (wrap_unicode) {
        /* A library built without libunibreak refuses the feature; say so before any track. */
        ASS_Track *probe = ass_new_track(ctx->library);
        int supported = probe && ass_track_set_feature(probe, ASS_FEATURE_WRAP_UNICODE, 1) == 0;
        if (probe) ass_free_track(probe);
        if (!supported) return -1;
    }
    ctx->wrap_unicode = wrap_unicode;
    /* Only enabling can fail, and it was just proven to work. */
    apply_features(ctx);
    return 0;
}

int plx_ass_set_cache_limits(PlxAss *ctx, int glyph_max, int bitmap_mib)
{
    if (!ctx || glyph_max < 1 || bitmap_mib < 1) return -1;
    ass_set_cache_limits(ctx->renderer, glyph_max, bitmap_mib);
    return 0;
}

int plx_ass_set_line_spacing(PlxAss *ctx, double spacing)
{
    if (!ctx || !isfinite(spacing) || fabs(spacing) > MAX_LINE_SPACING) return -1;
    ass_set_line_spacing(ctx->renderer, spacing);
    /* libass compares images, not settings: redraw the next frame in full. */
    ctx->cache_valid = 0;
    return 0;
}

int plx_ass_stats(const PlxAss *ctx, PlxAssStats *out)
{
    if (!ctx || !out) return -1;
    out->library_fonts = (uint32_t)plxi_library_fonts(ctx->library);
    out->font_scans = ctx->font_scans;
    out->tracks = ctx->tracks;
    out->last_libass_us = ctx->last_libass_us;
    out->last_composite_us = ctx->last_composite_us;
    out->last_libass_cpu_us = ctx->last_libass_cpu_us;
    out->last_composite_cpu_us = ctx->last_composite_cpu_us;
    out->last_images = ctx->last_images;
    out->last_image_pixels = ctx->last_image_pixels;
    return 0;
}

int plx_ass_chunk(PlxAss *ctx, const uint8_t *data, size_t len,
                  int64_t start_ms, int64_t duration_ms)
{
    if (!ctx || !ctx->track || !data || !len || len > MAX_EVENT_BYTES ||
        duration_ms <= 0 || start_ms > INT64_MAX - duration_ms ||
        ctx->track->n_events >= MAX_EVENTS)
        return -1;
    ass_process_chunk(ctx->track, (char *)data, (int)len, start_ms, duration_ms);
    return 0;
}

void plx_ass_prune_before(PlxAss *ctx, int64_t deadline_ms)
{
    if (ctx && ctx->track) ass_prune_events(ctx->track, deadline_ms);
}

void plx_ass_flush_events(PlxAss *ctx)
{
    if (ctx && ctx->track) ass_flush_events(ctx->track);
}

static int clipped_bounds(const ASS_Image *image, int width, int height,
                           int *left, int *top, int *right, int *bottom)
{
    if (image->w <= 0 || image->h <= 0) return 0;
    if (!image->bitmap || image->stride < image->w ||
        image->w > 32768 || image->h > 32768) return -1;
    int64_t r = (int64_t)image->dst_x + image->w;
    int64_t b = (int64_t)image->dst_y + image->h;
    *left = image->dst_x < 0 ? 0 : image->dst_x;
    *top = image->dst_y < 0 ? 0 : image->dst_y;
    *right = r > width ? width : (int)r;
    *bottom = b > height ? height : (int)b;
    return *right > *left && *bottom > *top;
}

struct Bounds { int left, top, right, bottom; };

static struct Bounds unite(struct Bounds a, struct Bounds b)
{
    return (struct Bounds){
        a.left < b.left ? a.left : b.left,
        a.top < b.top ? a.top : b.top,
        a.right > b.right ? a.right : b.right,
        a.bottom > b.bottom ? a.bottom : b.bottom
    };
}

static size_t region_bytes(struct Bounds b)
{
    return (size_t)(b.right - b.left) * (b.bottom - b.top) * 4;
}

/* Merge to a fixed point: a union can overlap a previously separate region.
 * Every image ends in exactly one region, so rendering the regions independently
 * preserves the upstream list's layer order, including translucent overlaps. */
static void add_bounds(struct Bounds *regions, size_t *count, struct Bounds box)
{
    for (size_t i = 0; i < *count;) {
        struct Bounds other = regions[i];
        if (box.left <= other.right && other.left <= box.right &&
            box.top <= other.bottom && other.top <= box.bottom) {
            box = unite(box, other);
            regions[i] = regions[--(*count)];
            i = 0;
        } else {
            ++i;
        }
    }
    /* An unusually fragmented script stays bounded without dropping any image. */
    if (*count == PLX_ASS_MAX_REGIONS) {
        for (size_t i = 0; i < *count; ++i) box = unite(box, regions[i]);
        *count = 0;
    }
    regions[(*count)++] = box;
}

static int same_image(struct ImageKey a, struct ImageKey b)
{
    return a.bitmap == b.bitmap && a.color == b.color && a.region == b.region &&
        a.x == b.x && a.y == b.y && a.width == b.width && a.height == b.height &&
        a.stride == b.stride;
}

/* The pinned libass compares bitmap identity in ass_image_compare and keeps
 * prev_images_root alive until AFTER it builds/compares the next frame. Its
 * immutable coverage buffer therefore cannot be freed and reused between these
 * consecutive snapshots. Store addresses only; never dereference an old one.
 * Unlike libass's whole-frame changed flag, these keys identify static regions
 * while another sign moves, changes colour, or advances its karaoke coverage. */
static int retained_regions(PlxAss *ctx, ASS_Image *images, int width, int height,
                            const struct Bounds *bounds, size_t count,
                            size_t image_count, int reusable, int *retained)
{
    reusable = reusable && ctx->region_count == count && ctx->image_count == image_count;
    for (size_t i = 0; i < count; ++i) {
        retained[i] = reusable &&
            ctx->regions[i].width == bounds[i].right - bounds[i].left &&
            ctx->regions[i].height == bounds[i].bottom - bounds[i].top;
    }
    if (ctx->image_capacity < image_count) {
        struct ImageKey *next = realloc(ctx->images, image_count * sizeof(*next));
        if (!next) return -1;
        ctx->images = next;
        ctx->image_capacity = image_count;
    }
    size_t index = 0;
    for (ASS_Image *p = images; p; p = p->next, ++index) {
        struct ImageKey key = { .region = SIZE_MAX };
        int l, t, r, b;
        if (clipped_bounds(p, width, height, &l, &t, &r, &b) > 0) {
            for (size_t i = 0; i < count; ++i) {
                struct Bounds box = bounds[i];
                if (l < box.left || t < box.top || r > box.right || b > box.bottom) continue;
                key = (struct ImageKey){
                    .bitmap = (uintptr_t)(p->bitmap + (size_t)(t - p->dst_y) * p->stride + (l - p->dst_x)),
                    .color = p->color, .region = i, .x = l - box.left, .y = t - box.top,
                    .width = r - l, .height = b - t, .stride = p->stride
                };
                break;
            }
            if (key.region == SIZE_MAX) return -1;
        }
        if (reusable && !same_image(ctx->images[index], key)) {
            size_t old_region = ctx->images[index].region;
            if (old_region < count) retained[old_region] = 0;
            if (key.region < count) retained[key.region] = 0;
        }
        ctx->images[index] = key;
    }
    ctx->image_count = image_count;
    return 0;
}

/* Keep one bounded arena. Regions retain their order on the reuse path, so
 * moving left in ascending order and then right in descending order cannot
 * overwrite another retained source. Clear changed regions only after moving. */
static void place_regions(PlxAss *ctx, const struct Bounds *bounds, size_t count,
                           const int *retained)
{
    size_t old_offset[PLX_ASS_MAX_REGIONS], next_offset[PLX_ASS_MAX_REGIONS];
    size_t old = 0, next = 0;
    for (size_t i = 0; i < count; ++i) {
        old_offset[i] = old;
        next_offset[i] = next;
        if (i < ctx->region_count) old += ctx->regions[i].bytes;
        next += region_bytes(bounds[i]);
    }
    for (size_t i = 0; i < count; ++i)
        if (retained[i] && next_offset[i] < old_offset[i])
            memmove(ctx->rgba + next_offset[i], ctx->rgba + old_offset[i], ctx->regions[i].bytes);
    for (size_t i = count; i-- > 0;)
        if (retained[i] && next_offset[i] > old_offset[i])
            memmove(ctx->rgba + next_offset[i], ctx->rgba + old_offset[i], ctx->regions[i].bytes);
    for (size_t i = 0; i < count; ++i) {
        struct Bounds b = bounds[i];
        PlxAssBitmap *bitmap = &ctx->regions[i];
        *bitmap = (PlxAssBitmap){
            .x = b.left, .y = b.top, .width = b.right - b.left, .height = b.bottom - b.top,
            .bytes = region_bytes(b),
            .rgba = ctx->rgba + next_offset[i]
        };
        if (!retained[i]) memset(ctx->rgba + next_offset[i], 0, bitmap->bytes);
    }
    ctx->region_count = count;
}

/* Test hook: ci/test_ass_regions.py defines this before including the file to count how often the
 * loop below steps onto a pixel that has nothing to draw (kind: `blend` in blend_row) instead of
 * skipping it with its neighbours. It is a no-op otherwise. */
#ifndef PLX_ASS_VISIT_ZERO
#define PLX_ASS_VISIT_ZERO(kind) ((void)0)
#endif
/* The same test hook for the NEON row path (ass_composite.h): `pixels` pixels were blended by one
 * vector operation, which does not pass through ass_blend_pixel. */
#ifndef PLX_ASS_VISIT_VECTOR
#define PLX_ASS_VISIT_VECTOR(pixels) ((void)0)
#endif

/* A pixel of one ASS_Image onto the premultiplied destination: `mask` is libass's coverage of it,
 * coverage[] the image's opacity table (coverage[0] is 0, and so can a nonzero entry be). */
static inline void blend_pixel(uint8_t *dst, unsigned mask, const uint8_t *coverage, uint32_t color)
{
    unsigned a = coverage[mask];
    if (!a) {
        PLX_ASS_VISIT_ZERO(blend);
        return;
    }
    if (a == 255) {
        ass_store_rgba(dst, color);
        return;
    }
    ass_store_rgba(dst, ass_blend_pixel(ass_load_rgba(dst), color, a));
}

static inline void blend_four(uint8_t *dst, const uint8_t *mask, const uint8_t *coverage,
                              uint32_t color)
{
    for (int i = 0; i < 4; ++i) blend_pixel(dst + 4 * i, mask[i], coverage, color);
}

/* One row of one ASS_Image onto the premultiplied destination: `n` pixels, mask[i] the libass
 * coverage of pixel i. Most of a sign's mask is zero, which draws nothing whatever the opacity, so
 * the row is read a group of bytes at a time and a group of zeros is passed over whole. The groups
 * come from the row's own start and never reach past its end (the stride's padding is not coverage).
 *
 * With NEON the group is eight pixels and one with any coverage is blended as a vector
 * (ass_blend8: the coverage computed from the image's opacity, which is coverage[255], then the
 * blend), zero lanes included: they come out as the destination, exactly. The pixels after the last
 * whole group, and every pixel on a target without NEON, go four at a time: a word of zeros is
 * passed over whole and a word with any coverage is drawn pixel by pixel, including the nonzero mask
 * bytes a faint opacity maps to coverage 0. */
static void blend_row(uint8_t *dst, const uint8_t *mask, int n, const uint8_t *coverage,
                      uint32_t color)
{
    int x = 0;
#ifdef PLX_ASS_NEON
    if (n >= 8) {
        /* The table maps 255 to the image's own opacity: (255 * o + 127) / 255 is o. */
        const uint8x8_t opacity = vdup_n_u8(coverage[255]);
        const AssSource8 source = ass_source8(color);
        for (; x + 8 <= n; x += 8) {
            /* Looked at in core registers: a NEON-to-core move stalls the pipeline. */
            uint64_t group;
            memcpy(&group, mask + x, sizeof group);
            if (!group) continue;
            PLX_ASS_VISIT_VECTOR(8);
            ass_blend8(dst + 4 * x, vld1_u8(mask + x), opacity, source);
        }
    }
#endif
    for (; x + 4 <= n; x += 4) {
        uint32_t word;
        memcpy(&word, mask + x, sizeof word);
        if (word) blend_four(dst + 4 * x, mask + x, coverage, color);
    }
    for (; x < n; ++x) blend_pixel(dst + 4 * x, mask[x], coverage, color);
}

static int compose_frame(PlxAss *ctx, ASS_Image *images, int width, int height, int reusable,
                         PlxAssFrame *frame);

int plx_ass_render(PlxAss *ctx, int64_t now_ms, int width, int height,
                   int storage_width, int storage_height,
                   PlxAssFrame *frame)
{
    if (!ctx || !ctx->track || !frame || width <= 0 || height <= 0 ||
        width > 4096 || height > 2160 || (size_t)width * height > MAX_PIXELS ||
        storage_width <= 0 || storage_height <= 0 ||
        storage_width > 65536 || storage_height > 65536)
        return -1;
    int resized = ctx->width != width || ctx->height != height ||
        ctx->storage_width != storage_width || ctx->storage_height != storage_height;
    if (resized) {
        ass_set_frame_size(ctx->renderer, width, height);
        /* Original coded raster, distinct from the rendered video rectangle.
         * libass uses this for pixel aspect, blur and 3D transform semantics. */
        ass_set_storage_size(ctx->renderer, storage_width, storage_height);
        ctx->width = width;
        ctx->height = height;
        ctx->storage_width = storage_width;
        ctx->storage_height = storage_height;
    }
    int reusable = ctx->cache_valid && !resized;
    /* Any failed call breaks the consecutive-frame identity proof above. */
    ctx->cache_valid = 0;
    int changed = 0;
    struct Lap wall, cpu;
    lap_start(&wall, CLOCK_MONOTONIC);
    lap_start(&cpu, CLOCK_THREAD_CPUTIME_ID);
    ASS_Image *images = ass_render_frame(ctx->renderer, ctx->track, now_ms, &changed);
    ctx->last_libass_us = lap_us(&wall);
    ctx->last_libass_cpu_us = lap_us(&cpu);
    ctx->last_composite_us = ctx->last_composite_cpu_us = 0;
    if (!changed && reusable && !ctx->first) {
        ctx->cache_valid = 1;
        return 0;
    }
    ctx->first = 0;
    int result = compose_frame(ctx, images, width, height, reusable, frame);
    ctx->last_composite_us = lap_us(&wall);
    ctx->last_composite_cpu_us = lap_us(&cpu);
    return result;
}

/* Everything plx_ass_render does with libass's image list: bounds, retained regions, the arena and
 * the per-pixel composite. Its wall time is last_composite_us, its thread CPU time
 * last_composite_cpu_us. */
static int compose_frame(PlxAss *ctx, ASS_Image *images, int width, int height, int reusable,
                         PlxAssFrame *frame)
{
    struct Bounds bounds[PLX_ASS_MAX_REGIONS];
    size_t region_count = 0;
    int image_count = 0;
    uint64_t image_pixels = 0;
    for (ASS_Image *p = images; p; p = p->next) {
        if (++image_count > MAX_IMAGES) return -1;
        int l, t, r, b;
        int valid = clipped_bounds(p, width, height, &l, &t, &r, &b);
        if (valid < 0) return -1;
        if (p->w > 0 && p->h > 0) image_pixels += (uint64_t)p->w * (uint64_t)p->h;
        if (!valid) continue;
        /* Transparent border preserves linear filtering at each new texture edge. */
        add_bounds(bounds, &region_count, (struct Bounds){
            l > 0 ? l - 1 : 0, t > 0 ? t - 1 : 0,
            r < width ? r + 1 : width, b < height ? b + 1 : height
        });
    }
    ctx->last_images = (uint32_t)image_count;
    ctx->last_image_pixels = image_pixels;
    memset(frame, 0, sizeof(*frame));
    if (!region_count) {
        ctx->region_count = ctx->image_count = 0;
        ctx->cache_valid = 1;
        return 1;
    }
    int retained[PLX_ASS_MAX_REGIONS];
    if (retained_regions(ctx, images, width, height, bounds, region_count,
                         (size_t)image_count, reusable, retained) < 0) return -1;
    size_t bytes = 0;
    for (size_t i = 0; i < region_count; ++i) {
        size_t n = region_bytes(bounds[i]);
        if (n > MAX_PIXELS * 4 || bytes > MAX_PIXELS * 4 - n) return -1;
        bytes += n;
    }
    /* One arena bounds retained capacity too: keeping a separate high-water
     * allocation per region could retain 64 full canvases as signs move. */
    if (ctx->bytes < bytes) {
        uint8_t *next = realloc(ctx->rgba, bytes);
        if (!next) return -1;
        ctx->rgba = next;
        ctx->bytes = bytes;
    }
    place_regions(ctx, bounds, region_count, retained);
    /* ASS_Image is an ordered list (shadow, outline, glyph, next event). Compose every layer in that
     * order in premultiplied space and publish the result as it is: PREMULTIPLIED RGBA, drawn by the
     * application with a premultiplied blend (gfx's `draw_tex_premultiplied`). A conversion to straight
     * RGBA here would be a pass over every pixel of every changed region, most of them transparent,
     * to undo what the blend already holds. */
    size_t image_index = 0;
    for (ASS_Image *p = images; p; p = p->next, ++image_index) {
        size_t region = ctx->images[image_index].region;
        if (region == SIZE_MAX || retained[region]) continue;
        int l, t, r, b;
        if (clipped_bounds(p, width, height, &l, &t, &r, &b) <= 0) continue;
        PlxAssBitmap *bitmap = &ctx->regions[region];
        uint32_t color = (p->color >> 24) | ((p->color >> 8) & 0xff00u) |
                         ((p->color << 8) & 0xff0000u) | 0xff000000u;
        unsigned opacity = 255 - (p->color & 255);
        uint8_t coverage[256];
        for (unsigned i = 0; i < 256; ++i)
            coverage[i] = (uint8_t)((i * opacity + 127) / 255);
        for (int y = t; y < b; ++y) {
            const uint8_t *mask = p->bitmap + (size_t)(y - p->dst_y) * p->stride;
            uint8_t *dst = (uint8_t *)bitmap->rgba +
                ((size_t)(y - bitmap->y) * bitmap->width + l - bitmap->x) * 4;
            blend_row(dst, mask + (l - p->dst_x), r - l, coverage, color);
        }
    }
    ctx->cache_valid = 1;
    frame->count = region_count;
    frame->regions = ctx->regions;
    return 1;
}
