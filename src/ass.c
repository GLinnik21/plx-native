/* Compiled INTO libass-plx, never linked into the app. The implementation reads
 * ASS_Image/ASS_Track through the exact pinned libass headers used to build it. */
#include "../include/ass.h"
#include "ass_composite.h"
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
    PlxAssBitmap regions[PLX_ASS_MAX_REGIONS];
    int width, height, storage_width, storage_height;
    int first;
    char *default_font;
    unsigned reciprocal[256];
};

/* Subtitle text and font metadata are untrusted and can be private. Do not
 * forward libass's per-event diagnostic output to the application's event log. */
static void quiet_message(int level, const char *format, va_list args, void *data)
{
    (void)level; (void)format; (void)args; (void)data;
}

unsigned plx_ass_abi_version(void) { return 2; }

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
    for (unsigned a = 1; a < 256; ++a) ctx->reciprocal[a] = 65536u / a;
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
    int changed = 0;
    ASS_Image *images = ass_render_frame(ctx->renderer, ctx->track, now_ms, &changed);
    if (!changed && !resized && !ctx->first) return 0;
    ctx->first = 0;
    struct Bounds bounds[PLX_ASS_MAX_REGIONS];
    size_t region_count = 0;
    int image_count = 0;
    for (ASS_Image *p = images; p; p = p->next) {
        if (++image_count > MAX_IMAGES) return -1;
        int l, t, r, b;
        int valid = clipped_bounds(p, width, height, &l, &t, &r, &b);
        if (valid < 0) return -1;
        if (!valid) continue;
        /* Transparent border preserves linear filtering at each new texture edge. */
        add_bounds(bounds, &region_count, (struct Bounds){
            l > 0 ? l - 1 : 0, t > 0 ? t - 1 : 0,
            r < width ? r + 1 : width, b < height ? b + 1 : height
        });
    }
    memset(frame, 0, sizeof(*frame));
    if (!region_count) return 1;
    size_t bytes = 0;
    for (size_t i = 0; i < region_count; ++i) {
        struct Bounds b = bounds[i];
        size_t n = (size_t)(b.right - b.left) * (b.bottom - b.top) * 4;
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
    memset(ctx->rgba, 0, bytes);
    size_t offset = 0;
    for (size_t i = 0; i < region_count; ++i) {
        struct Bounds b = bounds[i];
        PlxAssBitmap *bitmap = &ctx->regions[i];
        bitmap->x = b.left;
        bitmap->y = b.top;
        bitmap->width = b.right - b.left;
        bitmap->height = b.bottom - b.top;
        bitmap->bytes = (size_t)bitmap->width * bitmap->height * 4;
        bitmap->rgba = ctx->rgba + offset;
        offset += bitmap->bytes;
    }
    /* ASS_Image is an ordered list (shadow, outline, glyph, next event). Compose
     * every layer in that order in premultiplied space before converting to the
     * straight RGBA expected by SDL's ordinary blend mode. */
    for (ASS_Image *p = images; p; p = p->next) {
        int l, t, r, b;
        if (clipped_bounds(p, width, height, &l, &t, &r, &b) <= 0) continue;
        PlxAssBitmap *bitmap = NULL;
        for (size_t i = 0; i < region_count; ++i) {
            struct Bounds box = bounds[i];
            if (l >= box.left && t >= box.top && r <= box.right && b <= box.bottom) {
                bitmap = &ctx->regions[i];
                break;
            }
        }
        if (!bitmap) return -1;
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
            for (int x = l; x < r; ++x, dst += 4) {
                unsigned a = coverage[mask[x - p->dst_x]];
                if (!a) continue;
                if (a == 255) {
                    ass_store_rgba(dst, color);
                    continue;
                }
                ass_store_rgba(dst, ass_blend_pixel(ass_load_rgba(dst), color, a));
            }
        }
    }
    for (size_t i = 0; i < bytes; i += 4) {
        unsigned a = ctx->rgba[i + 3];
        /* Transparent pixels are already zero; opaque pixels are already
         * straight RGBA. Avoid three variable divisions for every opaque pixel
         * (software division on the ARMv7 target), including static vector signs. */
        if (!a || a == 255) continue;
        for (int c = 0; c < 3; ++c) {
            ctx->rgba[i + c] = ass_straight_channel(ctx->rgba[i + c], a,
                                                  ctx->reciprocal[a]);
        }
    }
    frame->count = region_count;
    frame->regions = ctx->regions;
    return 1;
}
