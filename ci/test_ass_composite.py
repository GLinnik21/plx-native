#!/usr/bin/env python3
"""Exhaustive native arithmetic check against the scalar compositing reference."""
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
            if (a) {
                unsigned expected = (s*255 + a/2)/a;
                if (expected > 255) expected = 255;
                if (ass_straight_channel(s, a, 65536/a) != expected) return 2;
            }
        }
    }
    puts("PASS: 16777216 blend combinations and 65280 alpha conversions match scalar RGBA");
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
