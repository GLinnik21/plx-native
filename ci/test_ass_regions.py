#!/usr/bin/env python3
"""Exercise the actual native region compositor with controlled libass images.

The producer stub supplies immutable coverage buffers, as pinned libass does.
Count real blend operations, then compare cached output with a fresh compositor
across motion, content changes, clipping, region merges, gaps and arena shifts.
Count the pixels the blend loop and the straight-alpha pass step onto one at a time
although they have nothing to draw (the compositor skips runs of them whole), and
compare the compositor's pixels with a frozen copy of the old scalar loops on random
masks. No native dependency build is needed for this host regression gate.
"""
from pathlib import Path
import os
import shlex
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
HEADER = r'''
#pragma once
#include <stdint.h>
#include <stdarg.h>
typedef struct { int unused; } ASS_Library;
typedef struct { int unused; } ASS_Renderer;
typedef struct { int track_type, n_styles, n_events; } ASS_Track;
typedef struct ass_image {
    int w, h, stride;
    unsigned char *bitmap;
    uint32_t color;
    int dst_x, dst_y;
    struct ass_image *next;
} ASS_Image;
#define TRACK_TYPE_UNKNOWN 0
#define ASS_FONTPROVIDER_NONE 0
static ASS_Image *ass_render_frame(ASS_Renderer *, ASS_Track *, long long, int *);
static ASS_Library *ass_library_init(void) { return NULL; }
static ASS_Renderer *ass_renderer_init(ASS_Library *l) { return NULL; }
static ASS_Track *ass_new_track(ASS_Library *l) { return NULL; }
static ASS_Track *ass_read_memory(ASS_Library *l, char *d, size_t n, char *e) { return NULL; }
static void ass_free_track(ASS_Track *t) {}
static void ass_renderer_done(ASS_Renderer *r) {}
static void ass_library_done(ASS_Library *l) {}
static void ass_set_message_cb(ASS_Library *l, void (*cb)(int,const char *,va_list,void *), void *d) {}
static void ass_set_extract_fonts(ASS_Library *l, int v) {}
static void ass_set_cache_limits(ASS_Renderer *r, int g, int b) {}
static void ass_add_font(ASS_Library *l, const char *n, const char *d, int z) {}
static void ass_process_codec_private(ASS_Track *t, char *d, int z) {}
static void ass_set_check_readorder(ASS_Track *t, int v) {}
static void ass_set_fonts(ASS_Renderer *r, const char *f, const char *n, int p, const char *c, int u) {}
static void ass_process_chunk(ASS_Track *t, char *d, int n, long long s, long long e) {}
static void ass_prune_events(ASS_Track *t, long long d) {}
static void ass_flush_events(ASS_Track *t) {}
static void ass_set_frame_size(ASS_Renderer *r, int w, int h) {}
#define ASS_FEATURE_WRAP_UNICODE 3
static int ass_track_set_feature(ASS_Track *t, int f, int e) { return 0; }
static void ass_set_line_spacing(ASS_Renderer *r, double s) {}
static size_t plxi_library_fonts(ASS_Library *l) { return 0; }
static void ass_set_storage_size(ASS_Renderer *r, int w, int h) {}
'''
SOURCE = r'''
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <ass/ass.h>
#include "ass_composite.h"

/* FROZEN: the scalar compositor as it was before the zero-run skipping (src/ass.c at 8c8020ae0,
 * compose_frame's two loops), run over a whole canvas instead of per region. Every pixel is visited
 * one at a time; this is the definition of the pixels the optimised code must produce. Do not
 * "improve" it, and keep it above the ass_blend_pixel counter below so it is not counted. */
static void reference_compose(const ASS_Image *list, int width, int height, uint8_t *canvas)
{
    unsigned reciprocal[256] = {0};
    for (unsigned a = 1; a < 256; ++a) reciprocal[a] = 65536u / a;
    for (const ASS_Image *p = list; p; p = p->next) {
        if (p->w <= 0 || p->h <= 0) continue;
        int l = p->dst_x < 0 ? 0 : p->dst_x, t = p->dst_y < 0 ? 0 : p->dst_y;
        long long right = (long long)p->dst_x + p->w, bottom = (long long)p->dst_y + p->h;
        int r = right > width ? width : (int)right, b = bottom > height ? height : (int)bottom;
        if (r <= l || b <= t) continue;
        uint32_t color = (p->color >> 24) | ((p->color >> 8) & 0xff00u) |
                         ((p->color << 8) & 0xff0000u) | 0xff000000u;
        unsigned opacity = 255 - (p->color & 255);
        uint8_t coverage[256];
        for (unsigned i = 0; i < 256; ++i)
            coverage[i] = (uint8_t)((i * opacity + 127) / 255);
        for (int y = t; y < b; ++y) {
            const uint8_t *mask = p->bitmap + (size_t)(y - p->dst_y) * p->stride;
            uint8_t *dst = canvas + ((size_t)y * width + l) * 4;
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
    for (size_t i = 0; i < (size_t)width * height * 4; i += 4) {
        unsigned a = canvas[i + 3];
        if (!a || a == 255) continue;
        for (int c = 0; c < 3; ++c)
            canvas[i + c] = ass_straight_channel(canvas[i + c], a, reciprocal[a]);
    }
}

static unsigned blends, zero_blend, zero_straight;
static uint32_t counted_blend(uint32_t dst, uint32_t color, unsigned a) {
    ++blends;
    return ass_blend_pixel(dst, color, a);
}
#define ass_blend_pixel counted_blend
#define PLX_ASS_VISIT_ZERO(kind) (++zero_##kind)
#include "ass.c"

static ASS_Image *input;
static ASS_Image *ass_render_frame(ASS_Renderer *r, ASS_Track *t, long long ms, int *changed) {
    *changed = 2; /* Another region can change while this one stays identical. */
    return input;
}
static PlxAss *context(void) {
    static ASS_Track track = {1, 1, 1};
    static ASS_Renderer renderer;
    PlxAss *ctx = calloc(1, sizeof(*ctx));
    assert(ctx);
    ctx->track = &track;
    ctx->renderer = &renderer;
    ctx->first = 1;
    for (unsigned a = 1; a < 256; ++a) ctx->reciprocal[a] = 65536u / a;
    return ctx;
}
enum { WIDTH = 96, HEIGHT = 48, CANVAS = WIDTH * HEIGHT * 4 };
static void render(PlxAss *ctx, int width, uint8_t *canvas) {
    PlxAssFrame f;
    int result = plx_ass_render(ctx, 0, width, HEIGHT, width, HEIGHT, &f);
    assert(result == 1);
    memset(canvas, 0, CANVAS);
    for (size_t i = 0; i < f.count; ++i) {
        const PlxAssBitmap *b = &f.regions[i];
        for (int y = 0; y < b->height; ++y)
            memcpy(canvas + ((b->y + y) * WIDTH + b->x) * 4,
                   b->rgba + y * b->width * 4, b->width * 4);
    }
}
static unsigned check(PlxAss *ctx, int width) {
    uint8_t actual[CANVAS], expected[CANVAS];
    blends = 0;
    render(ctx, width, actual);
    unsigned cached = blends;
    PlxAss *fresh = context();
    blends = 0;
    render(fresh, width, expected);
    assert(memcmp(actual, expected, CANVAS) == 0);
    plx_ass_destroy(fresh);
    return cached;
}
static uint32_t rng = 0x13280001u;
static uint32_t rnd32(void) {
    rng ^= rng << 13; rng ^= rng >> 17; rng ^= rng << 5;
    return rng;
}
static unsigned below(unsigned n) { return rnd32() % n; }
/* A mask of `h` rows of `stride` bytes (the last row only `w` long, as libass allocates it): runs of
 * zeros of every length 0..40 between runs of coverage 255, 1, 2..4 or random, so a zero run starts
 * at every alignment. The padding between rows is nonzero garbage no image may read. */
static void fill_mask(uint8_t *mask, int w, int h, int stride) {
    for (int y = 0; y < h; ++y) {
        uint8_t *row = mask + (size_t)y * stride;
        int x = 0, zeros = (int)below(2);
        while (x < w) {
            int len = zeros ? (int)below(41) : 1 + (int)below(12), kind = (int)below(4);
            for (int i = 0; i < len && x + i < w; ++i)
                row[x + i] = zeros ? 0 : kind == 0 ? 255 : kind == 1 ? 1 : kind == 2 ? 2 + below(3) : 1 + below(255);
            x += len;
            zeros ^= 1;
        }
        if (y < h - 1)
            for (int i = w; i < stride; ++i) row[i] = 1 + below(255);
    }
}
/* Random images over a small canvas: clipped on every side, overlapping, opacities from fully
 * opaque to fully transparent (opacity 1 maps every coverage below 128 to 0: a nonzero mask byte
 * that draws nothing). Each frame is composed by the real compositor and by the frozen scalar loops. */
static void random_frames(int frames) {
    static const unsigned alphas[] = {0, 0, 1, 2, 127, 254, 255};
    for (int f = 0; f < frames; ++f) {
        ASS_Image images[5];
        uint8_t *masks[5];
        int count = 1 + (int)below(5);
        for (int i = 0; i < count; ++i) {
            int w = 1 + (int)below(70), h = 1 + (int)below(5), stride = w + (int)below(10);
            masks[i] = malloc((size_t)stride * (h - 1) + w); /* exact: a read past the row end is out of bounds */
            assert(masks[i]);
            fill_mask(masks[i], w, h, stride);
            unsigned alpha = below(8) == 7 ? below(256) : alphas[below(7)];
            images[i] = (ASS_Image){w, h, stride, masks[i], (rnd32() & ~255u) | alpha,
                                    (int)below(WIDTH + 41) - 30, (int)below(HEIGHT + 4) - 3,
                                    i + 1 < count ? &images[i + 1] : NULL};
        }
        input = &images[0];
        PlxAss *ctx = context();
        uint8_t actual[CANVAS], expected[CANVAS];
        memset(expected, 0, CANVAS);
        render(ctx, WIDTH, actual);
        reference_compose(input, WIDTH, HEIGHT, expected);
        if (memcmp(actual, expected, CANVAS) != 0) {
            fprintf(stderr, "random frame %d differs from the scalar reference\n", f);
            exit(1);
        }
        plx_ass_destroy(ctx);
        for (int i = 0; i < count; ++i) free(masks[i]);
    }
}
/* Pixels each loop stepped onto singly although it had nothing to draw, for one image. */
static void zero_visits(ASS_Image *image, unsigned *blend, unsigned *straight) {
    input = image;
    PlxAss *ctx = context();
    uint8_t canvas[CANVAS];
    zero_blend = zero_straight = 0;
    render(ctx, WIDTH, canvas);
    *blend = zero_blend;
    *straight = zero_straight;
    plx_ass_destroy(ctx);
}

int main(void) {
    /* Zero runs are skipped whole. A sign's coverage is mostly zero (44% of the iterations of the old
     * blend loop on the #1328 fixture) and so is its region (70% of its pixels have alpha 0). Eight
     * rows of 24 zeros, 8 coverage-200 bytes, 32 zeros: no zero byte shares a group of four with
     * coverage, so no zero byte may be visited singly in the blend loop; the straight pass may step
     * onto the zeros around the sign's ends (the four-pixel groups the run edges fall into, the row
     * tail) but not onto the runs between. */
    {
        uint8_t mask[64 * 8];
        for (int y = 0; y < 8; ++y)
            for (int x = 0; x < 64; ++x) mask[y * 64 + x] = x >= 24 && x < 32 ? 200 : 0;
        ASS_Image sign = {64, 8, 64, mask, 0x11223300, 8, 6, NULL};
        unsigned blend, straight;
        zero_visits(&sign, &blend, &straight);
        unsigned zero_pixels = 66 * 10 - 8 * 8; /* the region is the sign and a transparent border */
        if (blend != 0 || straight > zero_pixels / 4) {
            fprintf(stderr, "zero runs were stepped through pixel by pixel: %u zero-coverage pixels in the "
                    "blend loop (want 0, the old loop: %u), %u zero-alpha pixels in the straight pass "
                    "(want at most %u of %u)\n", blend, 56u * 8, straight, zero_pixels / 4, zero_pixels);
            return 1;
        }
    }
    /* Zero runs of 37 and 24 around a 9-byte run of coverage 255, starting off every group boundary:
     * each row may step onto the zeros of the two groups the run's ends fall into and onto its tail
     * (at most 3 each), no more. */
    {
        uint8_t mask[80 * 5 + 70];
        for (int y = 0; y < 6; ++y)
            for (int x = 0; x < 70; ++x) mask[y * 80 + x] = x >= 37 && x < 46 ? 255 : 0;
        ASS_Image sign = {70, 6, 80, mask, 0x44556600, 3, 20, NULL};
        unsigned blend, straight;
        zero_visits(&sign, &blend, &straight);
        unsigned blend_bound = 6 * 9, straight_bound = 6 * 6 + 8 * 3; /* sign rows; region rows */
        if (blend > blend_bound || straight > straight_bound) {
            fprintf(stderr, "a misaligned sign was stepped through pixel by pixel: %u zero-coverage pixels in "
                    "the blend loop (at most %u), %u zero-alpha pixels in the straight pass (at most %u)\n",
                    blend, blend_bound, straight, straight_bound);
            return 1;
        }
    }
    random_frames(4000);
    uint8_t masks[2][256];
    for (int i = 0; i < 256; ++i) {
        masks[0][i] = (uint8_t)(40 + i % 170);
        masks[1][i] = (uint8_t)(210 - i % 170);
    }
    ASS_Image tail = {7, 4, 16, masks[0], 0xff00ff00, 75, 32, NULL};
    ASS_Image motion = {5, 5, 16, masks[0], 0x0000ff00, 40, 20, &tail};
    ASS_Image outline = {4, 3, 16, masks[1], 0x00ff0040, 7, 8, &motion};
    ASS_Image sign = {12, 8, 16, masks[0], 0xff000020, 5, 6, &outline};
    input = &sign;
    PlxAss *ctx = context();
    unsigned all = check(ctx, WIDTH);
    motion.dst_x += 2;
    motion.color ^= 0x10000000;
    unsigned changed = check(ctx, WIDTH);
    if (changed != (unsigned)(motion.w * motion.h)) {
        fprintf(stderr, "static regions were recomposited: %u blends, full frame %u\n", changed, all);
        return 1;
    }
    /* Both expansion and contraction move retained regions within one arena. */
    for (int i = 0; i < 240; ++i) {
        sign.w = 7 + i % 8;
        motion.w = 3 + (i * 3) % 10;
        motion.bitmap = masks[(i / 3) % 2];
        motion.dst_x = 38 + i % 7;
        outline.dst_x = 7 + (i / 9) % 3;
        check(ctx, WIDTH);
    }
    motion.dst_x = 8; motion.dst_y = 7; check(ctx, WIDTH); /* merge */
    motion.dst_x = 40; motion.dst_y = 20; check(ctx, WIDTH); /* split */
    sign.dst_x = -4; check(ctx, WIDTH); /* clipped mask origin */
    check(ctx, 78); /* canvas clips the tail and invalidates previous geometry */
    input = NULL; check(ctx, WIDTH); /* gap */
    input = &sign; check(ctx, WIDTH);
    sign.next = &motion; check(ctx, WIDTH); /* fewer images */
    sign.next = &outline; check(ctx, WIDTH);
    sign.w = 0; check(ctx, WIDTH); /* invisible image */
    sign.w = 12; check(ctx, WIDTH);
    /* An intervening failed frame retires the pointer-identity cache. The next
     * libass call may reuse an address whose old image is no longer retained. */
    sign.bitmap = NULL;
    PlxAssFrame failed;
    assert(plx_ass_render(ctx, 0, WIDTH, HEIGHT, WIDTH, HEIGHT, &failed) == -1);
    sign.bitmap = masks[0]; masks[0][0] ^= 15;
    check(ctx, WIDTH);
    plx_ass_destroy(ctx);
    puts("PASS: unchanged regions skip blending; zero runs are skipped; 253 frames match fresh composition; 4000 random frames match the scalar loops");
    return 0;
}
'''

with tempfile.TemporaryDirectory(prefix='plx-ass-regions-') as directory:
    folder = Path(directory)
    (folder / 'ass').mkdir()
    (folder / 'ass' / 'ass.h').write_text(HEADER)
    source, binary = folder / 'test.c', folder / 'test'
    source.write_text(SOURCE)
    subprocess.run([*shlex.split(os.environ.get('HOST_CC', 'cc')), '-std=gnu99', '-O2',
                    '-Wall', '-Wextra', '-Werror', '-Wno-unused-parameter',
                    '-I' + str(folder), '-I' + str(ROOT / 'src'),
                    str(source), '-o', str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
