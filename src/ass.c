/* Compiled INTO libass-plx, never linked into the app. The implementation reads
 * ASS_Image/ASS_Track through the exact pinned libass headers used to build it. */
#include "../include/ass.h"
#include <ass/ass.h>
#include <limits.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>

#define MAX_SCRIPT_BYTES (8u * 1024u * 1024u)
#define MAX_FONT_BYTES (64u * 1024u * 1024u)
#define MAX_EVENT_BYTES (256u * 1024u)
#define MAX_EVENTS 20000
#define MAX_PIXELS (3840u * 2160u)
#define MAX_IMAGES 16384

struct PlxAss {
    ASS_Library *library;
    ASS_Renderer *renderer;
    ASS_Track *track;
    uint8_t *rgba;
    size_t bytes, font_bytes;
    int width, height, storage_width, storage_height;
    int first;
    char *default_font;
};

/* Subtitle text and font metadata are untrusted and can be private. Do not
 * forward libass's per-event diagnostic output to the application's event log. */
static void quiet_message(int level, const char *format, va_list args, void *data)
{
    (void)level; (void)format; (void)args; (void)data;
}

unsigned plx_ass_abi_version(void) { return 1; }

void plx_ass_destroy(PlxAss *ctx)
{
    if (!ctx) return;
    if (ctx->track) ass_free_track(ctx->track);
    if (ctx->renderer) ass_renderer_done(ctx->renderer);
    if (ctx->library) ass_library_done(ctx->library);
    free(ctx->rgba);
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
    /* libass bounds its glyph/bitmap caches, in glyphs / MiB respectively. */
    ass_set_cache_limits(ctx->renderer, 1000, 24);
    ctx->first = 1;
    return ctx;
failed:
    plx_ass_destroy(ctx);
    return NULL;
}

int plx_ass_add_font(PlxAss *ctx, const char *name, const uint8_t *data, size_t len)
{
    if (!ctx || ctx->track || !name || !*name || !data || !len ||
        len > MAX_FONT_BYTES || ctx->font_bytes > MAX_FONT_BYTES - len)
        return -1;
    ass_add_font(ctx->library, name, (char *)data, (int)len);
    ctx->font_bytes += len;
    return 0;
}

int plx_ass_load(PlxAss *ctx, const uint8_t *data, size_t len, int script)
{
    if (!ctx || ctx->track || !data || !len || len > MAX_SCRIPT_BYTES) return -1;
    if (script) {
        ctx->track = ass_read_memory(ctx->library, (char *)data, len, NULL);
    } else {
        ctx->track = ass_new_track(ctx->library);
        if (ctx->track) ass_process_codec_private(ctx->track, (char *)data, (int)len);
    }
    if (!ctx->track || ctx->track->track_type == TRACK_TYPE_UNKNOWN ||
        ctx->track->n_styles <= 0 || ctx->track->n_events > MAX_EVENTS ||
        (script && ctx->track->n_events == 0))
        return -1;
    /* Matroska ReadOrder is the deduplication key, including after demux replay. */
    ass_set_check_readorder(ctx->track, 1);
    /* Inter is the shipped Latin family. With provider NONE libass tries the
     * exact authored family, this family, then default_font (our CJK face). */
    ass_set_fonts(ctx->renderer, ctx->default_font, "Inter",
                  ASS_FONTPROVIDER_NONE, NULL, 1);
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

int plx_ass_render(PlxAss *ctx, int64_t now_ms, int width, int height,
                   int storage_width, int storage_height,
                   PlxAssBitmap *bitmap)
{
    if (!ctx || !ctx->track || !bitmap || width <= 0 || height <= 0 ||
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
    int changed = 0;
    ASS_Image *images = ass_render_frame(ctx->renderer, ctx->track, now_ms, &changed);
    if (!changed && !resized && !ctx->first) return 0;
    ctx->first = 0;
    int x0 = width, y0 = height, x1 = 0, y1 = 0, count = 0;
    for (ASS_Image *p = images; p; p = p->next) {
        if (++count > MAX_IMAGES) return -1;
        int l, t, r, b;
        int valid = clipped_bounds(p, width, height, &l, &t, &r, &b);
        if (valid < 0) return -1;
        if (!valid) continue;
        if (l < x0) x0 = l;
        if (t < y0) y0 = t;
        if (r > x1) x1 = r;
        if (b > y1) y1 = b;
    }
    memset(bitmap, 0, sizeof(*bitmap));
    if (x1 <= x0 || y1 <= y0) return 1;
    size_t bytes = (size_t)(x1 - x0) * (y1 - y0) * 4;
    if (ctx->bytes < bytes) {
        uint8_t *next = realloc(ctx->rgba, bytes);
        if (!next) return -1;
        ctx->rgba = next;
        ctx->bytes = bytes;
    }
    memset(ctx->rgba, 0, bytes);
    /* ASS_Image is an ordered list (shadow, outline, glyph, next event). Compose
     * every layer in that order in premultiplied space before converting to the
     * straight RGBA expected by SDL's ordinary blend mode. */
    for (ASS_Image *p = images; p; p = p->next) {
        int l, t, r, b;
        if (clipped_bounds(p, width, height, &l, &t, &r, &b) <= 0) continue;
        unsigned color[3] = {p->color >> 24, (p->color >> 16) & 255,
                             (p->color >> 8) & 255};
        unsigned opacity = 255 - (p->color & 255);
        for (int y = t; y < b; ++y) {
            const uint8_t *mask = p->bitmap + (size_t)(y - p->dst_y) * p->stride;
            uint8_t *dst = ctx->rgba + ((size_t)(y - y0) * (x1 - x0) + l - x0) * 4;
            for (int x = l; x < r; ++x, dst += 4) {
                unsigned a = (mask[x - p->dst_x] * opacity + 127) / 255;
                unsigned inverse = 255 - a;
                for (int c = 0; c < 3; ++c)
                    dst[c] = (uint8_t)((color[c] * a + dst[c] * inverse + 127) / 255);
                dst[3] = (uint8_t)(a + (dst[3] * inverse + 127) / 255);
            }
        }
    }
    for (size_t i = 0; i < bytes; i += 4) {
        unsigned a = ctx->rgba[i + 3];
        if (!a) continue;
        for (int c = 0; c < 3; ++c) {
            unsigned v = (ctx->rgba[i + c] * 255u + a / 2) / a;
            ctx->rgba[i + c] = (uint8_t)(v > 255 ? 255 : v);
        }
    }
    bitmap->x = x0;
    bitmap->y = y0;
    bitmap->width = x1 - x0;
    bitmap->height = y1 - y0;
    bitmap->bytes = bytes;
    bitmap->rgba = ctx->rgba;
    return 1;
}
