#!/usr/bin/env python3
"""Exhaustive native arithmetic check against the scalar compositing reference.

The scalar blend (`ass_blend_pixel`) is checked against the formula on every (coverage, source,
destination) triple. Where the host has NEON (`__ARM_NEON`: the 32-bit ARM TVs and arm64 Macs) the
eight-pixel kernel the row loop uses (`ass_blend8`) is checked the same way, lane by lane with a
different pixel in every lane, so a wrong rounding or a lane that leaks into its neighbour fails; a
host without NEON prints that the vector kernel was not exercised.
"""
from pathlib import Path
import os
import shlex
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SOURCE = r'''
#include "ass_composite.h"
#include <stdio.h>

int main(void) {
    for (unsigned a = 0; a < 256; ++a) {
        for (unsigned s = 0; s < 256; ++s) {
            for (unsigned d = 0; d < 256; ++d) {
                /* Other lanes vary too, exercising carries across packed channels. */
                unsigned src[4] = {s, 255-s, s ^ 85, 255};
                unsigned dst[4] = {d, d ^ 170, 255-d, d};
                uint8_t bytes[4] = {dst[0], dst[1], dst[2], dst[3]};
                uint32_t color = src[0] | (src[1]<<8) | (src[2]<<16) | (src[3]<<24);
                uint32_t packed = ass_blend_pixel(ass_load_rgba(bytes), color, a);
                ass_store_rgba(bytes, packed);
                for (unsigned c = 0; c < 4; ++c) {
                    unsigned expected = (src[c]*a + dst[c]*(255-a) + 127)/255;
                    if (bytes[c] != expected) {
                        fprintf(stderr, "blend a=%u s=%u d=%u c=%u: %u != %u\n",
                                a, s, d, c, bytes[c], expected);
                        return 1;
                    }
                }
            }
        }
    }
    puts("PASS: 16777216 blend combinations match scalar RGBA");
#ifdef PLX_ASS_NEON
    /* The rounding primitive: every numerator a blend can form (at most 255*255) divides as the
     * scalar's (n + 127) / 255, eight lanes at a time. */
    for (unsigned base = 0; base <= 65025; base += 8) {
        uint16_t in[8];
        uint8_t out[8];
        for (unsigned k = 0; k < 8; ++k) in[k] = base + k <= 65025 ? base + k : 0;
        vst1_u8(out, ass_div255_lanes(vld1q_u16(in)));
        for (unsigned k = 0; k < 8; ++k)
            if (out[k] != (in[k] + 127) / 255) {
                fprintf(stderr, "div255 n=%u: %u != %u\n", in[k], out[k], (in[k] + 127) / 255);
                return 1;
            }
    }
    /* The kernel at opacity 255, where the coverage is the mask byte itself: every (a, s, d) triple
     * reaches every lane, and each lane holds a different one from its neighbours (a and d step by
     * lane-dependent amounts, both bijections over a0 and d0), channel by channel as above. */
    for (unsigned s = 0; s < 256; ++s) {
        const unsigned src[4] = {s, 255 - s, s ^ 85, 255};
        const AssSource8 source = ass_source8(src[0] | (src[1] << 8) | (src[2] << 16) |
                                              ((uint32_t)src[3] << 24));
        for (unsigned a0 = 0; a0 < 256; ++a0) {
            for (unsigned d0 = 0; d0 < 256; ++d0) {
                uint8_t mask[8], dst[32], expected[32];
                for (unsigned k = 0; k < 8; ++k) {
                    const unsigned a = (a0 + 37 * k) & 255, d = (d0 + 91 * k) & 255;
                    const unsigned dc[4] = {d, d ^ 170, 255 - d, d};
                    mask[k] = (uint8_t)a;
                    for (unsigned c = 0; c < 4; ++c) {
                        dst[4 * k + c] = (uint8_t)dc[c];
                        expected[4 * k + c] = (uint8_t)((src[c] * a + dc[c] * (255 - a) + 127) / 255);
                    }
                }
                ass_blend8(dst, vld1_u8(mask), vdup_n_u8(255), source);
                for (unsigned i = 0; i < 32; ++i)
                    if (dst[i] != expected[i]) {
                        fprintf(stderr, "ass_blend8 s=%u a0=%u d0=%u lane %u channel %u: %u != %u\n",
                                s, a0, d0, i / 4, i % 4, dst[i], expected[i]);
                        return 1;
                    }
            }
        }
    }
    /* The coverage the kernel computes, (mask * opacity + 127) / 255, is the scalar code's table for
     * every opacity and mask byte: here the opacity varies, with a source and destination that
     * change with them. */
    for (unsigned opacity = 0; opacity < 256; ++opacity) {
        for (unsigned m0 = 0; m0 < 256; m0 += 8) {
            for (unsigned variant = 0; variant < 16; ++variant) {
                const unsigned s = (opacity * 7 + m0 * 3 + variant * 29) & 255;
                const unsigned src[4] = {s, 255 - s, s ^ 85, 255};
                const AssSource8 source = ass_source8(src[0] | (src[1] << 8) | (src[2] << 16) |
                                                      ((uint32_t)src[3] << 24));
                uint8_t mask[8], dst[32], expected[32];
                for (unsigned k = 0; k < 8; ++k) {
                    const unsigned m = m0 + k, a = (m * opacity + 127) / 255;
                    const unsigned d = (m * 5 + opacity * 11 + variant * 53 + k) & 255;
                    const unsigned dc[4] = {d, d ^ 170, 255 - d, 255 - (d ^ 85)};
                    mask[k] = (uint8_t)m;
                    for (unsigned c = 0; c < 4; ++c) {
                        dst[4 * k + c] = (uint8_t)dc[c];
                        expected[4 * k + c] = (uint8_t)((src[c] * a + dc[c] * (255 - a) + 127) / 255);
                    }
                }
                ass_blend8(dst, vld1_u8(mask), vdup_n_u8((uint8_t)opacity), source);
                if (memcmp(dst, expected, sizeof dst) != 0) {
                    fprintf(stderr, "ass_blend8 opacity=%u mask %u..%u variant %u differs from the "
                            "scalar coverage table\n", opacity, m0, m0 + 7, variant);
                    return 1;
                }
            }
        }
    }
    puts("PASS: NEON ass_blend8 matches scalar RGBA on 16777216 (coverage, source, destination) "
         "triples per lane and on every (mask, opacity) pair");
#else
    puts("NOTE: this host has no NEON: the vector kernel (ass_blend8) was NOT exercised, "
         "only the scalar blend");
#endif
    return 0;
}
'''

with tempfile.TemporaryDirectory(prefix="plx-ass-composite-") as directory:
    source = Path(directory) / "test.c"
    binary = Path(directory) / "test"
    source.write_text(SOURCE)
    subprocess.run([*shlex.split(os.environ.get("HOST_CC", "cc")), "-std=c99", "-O2",
                    "-Wall", "-Wextra", "-Werror", "-I" + str(ROOT / "src"),
                    str(source), "-o", str(binary)], check=True)
    subprocess.run([str(binary)], check=True)
