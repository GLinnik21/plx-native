#!/usr/bin/env python3
"""
font-hint-audit.py — verify the theme::size ladder rasterizes with design-true stroke
weights for the fonts we ship.

Why this exists: TrueType hinting grid-fits strokes to whole pixels, and the rounding is
size- and font-specific. Under FreeType's default NORMAL hinting, Arial's own bytecode
rounds horizontal bars UP — at bold 26 (size::LABEL, the card-title rung) it drew 4px bars
over 3px stems, inverting the typeface's stem>bar design and making titles read top-heavy.
The app therefore rasterizes with LIGHT hinting and draws text quads at integer pixel
origins (both in rust-modules/src/text.rs), which keeps stems >= bars at every size.

This script proves that property holds: it parses the rungs out of theme.rs `mod size`
(single source of truth), adds the two documented carve-outs, and renders a capital F from
each shipped font at each size under LIGHT hinting, measuring the solid core (coverage
> 0.9) of the top bar vs the vertical stem. Any size where bars come out heavier is
flagged; new flags (outside the accepted list below) fail the run.

Run it after swapping font files or touching the hinting/rasterization path:
    ./tools/font-hint-audit.py
Dependency: freetype-py  (pip install freetype-py — any venv works, no other deps)
"""

import os
import re
import sys

try:
    import freetype
except ImportError:
    sys.exit("freetype-py not installed — run: pip install freetype-py")

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
THEME = os.path.join(REPO, "rust-modules", "src", "ui", "theme.rs")
FONTS = {
    "bold": os.path.join(REPO, "pkg", "appfont-bold.ttf"),
    "regular": os.path.join(REPO, "pkg", "appfont.ttf"),
}
# Named carve-outs living outside the ladder (see theme.rs's size-module doc).
CARVEOUTS = {54: "HUD_TITLE_SZ (bold)", 36: "subtitle caption (bold)"}
# Known, accepted imbalances — reviewed 2026-07-18. MICRO (22) and LABEL's rare regular
# sites (26) are bar-heavy by one solid pixel even under LIGHT hinting; both are low-
# prominence roles and every remedy (size nudge) costs scale hierarchy. Anything NOT in
# this set that measures bar-heavy is a regression and fails the audit.
ACCEPTED = {("regular", 22), ("regular", 26)}


def ladder():
    """size -> rung name, parsed from theme.rs `mod size`."""
    src = open(THEME).read()
    mod = re.search(r"pub mod size \{(.*?)\n\}", src, re.S).group(1)
    return {int(m.group(2)): m.group(1)
            for m in re.finditer(r"pub const (\w+): c_int = (\d+);", mod)}


def measure_f(face, size):
    """Solid-core px of the capital F's top bar (rows) and stem (cols) + AA partials."""
    face.set_pixel_sizes(0, size)
    face.load_char("F", freetype.FT_LOAD_RENDER | freetype.FT_LOAD_TARGET_LIGHT)
    bmp = face.glyph.bitmap
    px = lambda x, y: bmp.buffer[y * bmp.pitch + x] / 255
    bar, bx = [], int(bmp.width * 0.6)
    for y in range(bmp.rows):
        v = px(bx, y)
        if v > 0.05:
            bar.append(v)
        elif bar:
            break
    yy = int(bmp.rows * 0.75)
    stem = [v for x in range(max(6, bmp.width // 2)) if (v := px(x, yy)) > 0.05]
    core = lambda p: sum(1 for v in p if v > 0.9)
    part = lambda p: sum(v for v in p if v <= 0.9)
    return core(bar), part(bar), core(stem), part(stem)


def main():
    names = ladder()
    sizes = sorted(set(names) | set(CARVEOUTS))
    failures = []
    for weight, path in FONTS.items():
        face = freetype.Face(path)
        print(f"== {weight}  ({os.path.basename(path)}) ==")
        print("size (rung)      | bar core+part | stem core+part | verdict")
        for sz in sizes:
            bc, bp, sc, sp = measure_f(face, sz)
            role = names.get(sz) or CARVEOUTS.get(sz, "")
            if bc > sc:
                verdict = "bars +%d  <-- AVOID" % (bc - sc)
                if (weight, sz) in ACCEPTED:
                    verdict += "  (accepted)"
                else:
                    failures.append((weight, sz))
            else:
                verdict = "ok (stems >= bars)"
            print(" %2d %-12s | %d + %.2f      | %d + %.2f       | %s"
                  % (sz, "(%s)" % role if role else "", bc, bp, sc, sp, verdict))
    if failures:
        print("\nFAIL: bar-heavy at", ", ".join("%s %d" % f for f in failures))
        return 1
    print("\nOK: every rung renders design-true (or is an accepted exception).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
