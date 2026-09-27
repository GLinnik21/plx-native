#!/usr/bin/env python3
"""Exercise the actual native region compositor with controlled libass images.

The producer stub supplies immutable coverage buffers, as pinned libass does.
Count real blend operations, then compare cached output with a fresh compositor
across motion, content changes, clipping, region merges, gaps and arena shifts.
No native dependency build is needed for this host regression gate.
"""
from pathlib import Path
import os
import shlex
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
HEADER = r'''
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
static void ass_set_storage_size(ASS_Renderer *r, int w, int h) {}
'''
SOURCE = r'''
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include "ass_composite.h"
static unsigned blends;
static uint32_t counted_blend(uint32_t dst, uint32_t color, unsigned a) {
    ++blends;
    return ass_blend_pixel(dst, color, a);
}
#define ass_blend_pixel counted_blend
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
int main(void) {
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
    puts("PASS: unchanged regions skip blending; 253 frames match fresh composition");
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
