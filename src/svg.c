// src/svg.c — runtime SVG rasterizer (nanosvg, vendored header-only, public domain).
//
// Ships vector icon *assets* that rasterize to an RGBA mask at the exact pixel size we
// draw them — the way iOS renders vector assets. Called from Rust (crate::svg) which
// uploads the result as a GL texture and tints it per state. Kept in C (like starfish.c)
// so nanosvg is compiled by the NDK's ARM gcc — pure-Rust SVG crates pull heavy SIMD
// deps that clash with this target's -neon / build-std constraints.
#include <stdlib.h>
#include <string.h>

#define NANOSVG_IMPLEMENTATION
#define NANOSVG_ALL_COLOR_KEYWORDS
#include "nanosvg.h"
#define NANOSVGRAST_IMPLEMENTATION
#include "nanosvgrast.h"

// Rasterize `svg` (len bytes, need not be NUL-terminated) into a freshly malloc'd w*h*4
// RGBA buffer, scaling the SVG to FIT w*h (uniform, centered). Author icons in #ffffff so
// the result is a white mask (alpha = coverage) that the shader tints. Returns NULL on any
// failure (bad args / parse / OOM). Free the result with svg_free.
unsigned char *svg_rasterize_rgba(const char *svg, int len, int w, int h) {
    if (!svg || len <= 0 || w <= 0 || h <= 0) return NULL;
    char *buf = (char *)malloc((size_t)len + 1);
    if (!buf) return NULL;
    memcpy(buf, svg, (size_t)len);
    buf[len] = 0;
    NSVGimage *img = nsvgParse(buf, "px", 96.0f); // nsvgParse mutates its input
    free(buf);
    if (!img) return NULL;

    float sw = img->width, sh = img->height;
    if (sw <= 0.0f || sh <= 0.0f) {
        nsvgDelete(img);
        return NULL;
    }
    float sx = (float)w / sw, sy = (float)h / sh;
    float scale = sx < sy ? sx : sy;                  // uniform fit
    float tx = ((float)w - sw * scale) * 0.5f;        // center
    float ty = ((float)h - sh * scale) * 0.5f;

    unsigned char *px = (unsigned char *)calloc((size_t)w * (size_t)h * 4, 1);
    NSVGrasterizer *r = nsvgCreateRasterizer();
    if (px && r) {
        nsvgRasterize(r, img, tx, ty, scale, px, w, h, w * 4);
    } else {
        free(px);
        px = NULL;
    }
    if (r) nsvgDeleteRasterizer(r);
    nsvgDelete(img);
    return px;
}

void svg_free(unsigned char *p) { free(p); }
