#!/usr/bin/env python3
"""Cut the shipped app icons from a single square logo master.

    python3 tools/mkicons.py assets/logo-master.png [--band=N] [--splash=assets/splash-master.png]

Emits `pkg/icon.png` (80), `pkg/largeIcon.png` (130) and, for the webosbrew channel listing,
`pkg/icon160.png` / `pkg/icon320.png`. With `--splash` it also emits `pkg/splash.png` at exactly
1920x1080, the size `splashBackground` requires and the size all four native-app splashes on the
dev TV actually are (Amazon, Apple TV, Netflix, YouTube — checked, since the field is documented
in a web-app context and it was not obvious it applies to `type: "native"` at all; it does).
Re-running this is the only supported way to change any of them.

Why a script and not four exports: the target is not "the logo, scaled". LG's icon guide
(webostv.developer.lge.com/develop/guides/icon) specifies a **126x126 background panel** with the
logo inside a **115x115 area plus >=5px padding** — so a master's own canvas margins may be wrong
by any amount, and exporting it at four sizes would silently inherit that. This measures the
master's INK and scales the master until the ink lands where LG wants it, so the result depends on
the logo rather than on how the master was cropped.

Two things measured off the TV's own store icons (`115x115` cache files pulled from
`/media/cryptofs/apps/usr/palm/applications/*/`), which is what the launcher actually renders:

  * **Real tiles are full-bleed opaque squares**, not transparent cut-outs — the launcher supplies
    the rounded corners. Apple TV and Spotify are on black, Netflix and YouTube on white, so a dark
    tile is on-style and needs no lightening.
  * **The logo occupies 70-91% of the tile width** (Apple TV 70, Spotify 74, Netflix 80, YouTube
    91). We take 78%, which lands mid-pack and satisfies the >=5px rule at every emitted size.

**It scales the whole master and crops, rather than cutting the logo out and pasting it onto a
flat panel.** The pasting version worked only because the first master's background was pure
`#000` everywhere; a master with a vignette, glow or gradient carries those pixels inside the ink
bbox and none outside it, so pasting stamps a visible rectangle of slightly-wrong background into
the tile — the same class of seam as a mismatched `iconColor`, and harder to spot because it is a
few levels rather than a colour. Scaling the whole canvas keeps the background continuous by
construction, at no cost when the background *is* flat.

`--band=N` selects one horizontal band of ink when the master is a stacked lockup (bands are
numbered from the top; the default uses all of them). This is a legibility lever, not a taste one:
a secondary line that is under ~10% of the ink height renders about **4 px tall at the 130 px
launcher size** and 2.5 px at 80 — below the point where a glyph is a glyph. Every reference icon
that carries readable text carries ONE line of it at aspect 3.1-4.2; the only reference with a 2:1
lockup (Apple TV) has no small text at all. The script prints each band's projected height so the
decision is made against a number.

It also prints the `iconColor` the master implies. That field paints the launcher tile *behind*
the icon, so when it disagrees with the icon's own background the tile shows a hard-edged
rectangle — shipped that way until 2026-08-02, gold tile under a black icon. `ci/check-package.py`
asserts the two agree.
"""
import re
import sys
from pathlib import Path

import numpy as np
from PIL import Image

# Fraction of the tile the logo's ink should span horizontally. Mid-pack against the four store
# icons measured above; also keeps >=5px padding at 130 (130*0.78 -> 101 wide, 14 px each side).
INK_FRACTION = 0.78
# (filename, size). 80/130 are LG's; 160/320 are the webosbrew channel's iconUri/detailIconUri.
TARGETS = (("icon.png", 80), ("largeIcon.png", 130), ("icon160.png", 160), ("icon320.png", 320))


def ink_bbox(a: np.ndarray, bg: np.ndarray, tol: int = 40) -> tuple:
    """Bounding box of everything that differs from the background colour."""
    ys, xs = np.where(np.abs(a - bg).sum(axis=2) > tol)
    if not len(xs):
        raise SystemExit("master is a flat colour — nothing to cut an icon from")
    return xs.min(), ys.min(), xs.max() + 1, ys.max() + 1


def row_bands(a: np.ndarray, bg: np.ndarray, tol: int = 40) -> list:
    """Contiguous runs of rows containing ink — the lockup's lines, top to bottom."""
    rows = (np.abs(a - bg).sum(axis=2) > tol).sum(axis=1)
    bands, start = [], None
    for y, v in enumerate(rows):
        if v and start is None:
            start = y
        elif not v and start is not None:
            bands.append((start, y))
            start = None
    if start is not None:
        bands.append((start, len(rows)))
    return bands


def sync_icon_color(appinfo: Path, hexcolor: str) -> str:
    """Point appinfo.json's iconColor at the master's background. Returns what it was."""
    text = appinfo.read_text()
    m = re.search(r'("iconColor"\s*:\s*")([^"]*)(")', text)
    if not m:
        raise SystemExit(f"no iconColor field in {appinfo}")
    if m.group(2).lower() != hexcolor:
        appinfo.write_text(text[:m.start(2)] + hexcolor + text[m.end(2):])
    return m.group(2)


def write_splash(src: Path, out: Path) -> None:
    """Emit the 1920x1080 splash. LANCZOS because the art is vector-derived with hard edges."""
    im = Image.open(src).convert("RGB")
    if im.size != (1920, 1080):
        w, h = im.size
        if abs(w / h - 16 / 9) > 0.01:
            raise SystemExit(f"splash master is {w}x{h} — not 16:9, so fitting it would letterbox "
                             f"or crop the artwork. Re-export it at 1920x1080.")
        print(f"  splash master is {w}x{h}; resampling to 1920x1080 ({1920 / w:.3f}x)")
        im = im.resize((1920, 1080), Image.LANCZOS)
    im.save(out, optimize=True)
    print(f"  {out.name:14s} 1920x1080")


def main() -> int:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    band_arg = next((a.split("=", 1)[1] for a in sys.argv[1:] if a.startswith("--band=")), None)
    splash = next((a.split("=", 1)[1] for a in sys.argv[1:] if a.startswith("--splash=")), None)
    if len(args) != 1:
        return print(__doc__) or 2

    repo = Path(__file__).resolve().parent.parent
    if splash:
        write_splash(Path(splash), repo / "pkg" / "splash.png")
    src = Image.open(args[0]).convert("RGB")
    a = np.asarray(src).astype(int)
    bg = a[2, 2].copy()                      # the master's own corner is the panel colour

    bands = row_bands(a, bg)
    if band_arg is not None:
        top, bot = bands[int(band_arg)]
    else:
        top, bot = bands[0][0], bands[-1][1]
    x0, _, x1, _ = ink_bbox(a[top:bot], bg)
    ink_w, ink_h = x1 - x0, bot - top
    cx, cy = (x0 + x1) / 2, (top + bot) / 2   # centre the TILE on the ink, not on the canvas
    hexcolor = "#%02x%02x%02x" % tuple(bg)
    print(f"  master {src.size[0]}x{src.size[1]}  panel {hexcolor}  bands {len(bands)}"
          f"{'' if band_arg is None else f' (using band {band_arg})'}  "
          f"-> ink {ink_w}x{ink_h} (aspect {ink_w / ink_h:.2f})")
    if len(bands) > 1 and band_arg is None:
        for i, (t, b) in enumerate(bands):
            at130 = (b - t) / ink_h * round(130 * INK_FRACTION) * ink_h / ink_w
            note = "   ** unreadable; consider --band **" if at130 < 8 else ""
            print(f"    band {i}: {b - t}px of {ink_h}  -> ~{at130:.1f}px tall at 130{note}")

    was = sync_icon_color(repo / "pkg" / "appinfo.json", hexcolor)
    print(f"  appinfo.json iconColor {was} -> {hexcolor}"
          + ("   (unchanged)" if was.lower() == hexcolor else "   ** CHANGED **"))

    for name, n in TARGETS:
        target_w = n * INK_FRACTION
        scale = target_w / ink_w
        if ink_h * scale > target_w:          # a tall mark is bounded by height instead
            scale = target_w / ink_h
        # Scale the WHOLE master, then take an n x n window centred on the ink — so whatever the
        # master's background does (vignette, glow, gradient) stays continuous across the tile.
        sw, sh = max(1, round(src.size[0] * scale)), max(1, round(src.size[1] * scale))
        # LANCZOS: these are downsamples of 4-13x, where a box filter visibly thins the strokes.
        scaled = src.resize((sw, sh), Image.LANCZOS)
        left, upper = round(cx * scale) - n / 2, round(cy * scale) - n / 2
        tile = Image.new("RGB", (n, n), tuple(bg))
        tile.paste(scaled, (round(-left), round(-upper)))
        out = repo / "pkg" / name
        tile.save(out, optimize=True)
        w, h = round(ink_w * scale), round(ink_h * scale)
        pad = min((n - w) // 2, (n - h) // 2)
        flag = "" if pad >= 5 else "   ** under LG's 5px minimum padding **"
        print(f"  {name:14s} {n}x{n}  logo {w}x{h}  padding {pad}px{flag}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
