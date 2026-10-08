#!/usr/bin/env python3
"""Render the landing page's glows: tiny, already blurred pictures that the browser scales up.

    python3 tools/render-site-glows.py [--only feel-glow.png ...]

`--only` renders just the named glows (`site_video.py adopt --derive` asks for the film's two).

A soft, coloured copy of the TV screen and of each close-up glows behind it. The page used to
make it live with `filter: blur(70px)` over the full-size image, which the browser re-renders
whenever the layer changes; a blur that heavy leaves nothing a 48-pixel picture cannot hold, so
each glow is now that picture (site/media/*-glow*.png), scaled up by the browser at no cost.

Each glow is baked for the box it fills on the page (at 1440 px wide, or 390 px for a
`-narrow` one): the source is cropped to the box's shape as `object-fit: cover` did, shrunk,
saturated as `saturate()` did, and blurred with the same radius relative to the box. The blur
spreads past the box, so the picture has a transparent margin, and its <img> is that much larger
than the box: the `--mx`/`--my` values this prints are what styles.css grows each box by. The
blur is done on premultiplied colour, as a browser's is, so the edge fades out without darkening.
Needs ffmpeg.
"""
import math
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
MEDIA = ROOT / "site" / "media"

# name, source, the box it fills (CSS px), blur radius (CSS px), saturation
GLOWS = [
    ("feel-glow.png", "feel-poster.jpg", (1047, 637), 70, 1.5),
    ("feel-glow-narrow.png", "feel-poster.jpg", (322, 207), 70, 1.5),
    ("closeup-glass-glow.png", "closeup-glass.jpg", (1091, 220), 64, 1.6),
    ("closeup-glass-narrow-glow.png", "closeup-glass-narrow.jpg", (308, 205), 64, 1.6),
    ("closeup-tiles-glow.png", "closeup-tiles.jpg", (532, 361), 64, 1.6),
    ("closeup-player-glow.png", "closeup-player.jpg", (532, 361), 64, 1.6),
]
# The blur radius in the picture's own pixels. Anything finer than the blur is gone anyway, so the
# picture is only as large as this allows; 3.5 px of Gaussian scales up with no visible steps.
SIGMA = 3.5


def saturate(s):
    """The colour matrix of CSS `saturate(s)` (Filter Effects 1, feColorMatrix type=saturate)."""
    m = {
        "rr": 0.213 + 0.787 * s, "rg": 0.715 - 0.715 * s, "rb": 0.072 - 0.072 * s,
        "gr": 0.213 - 0.213 * s, "gg": 0.715 + 0.285 * s, "gb": 0.072 - 0.072 * s,
        "br": 0.213 - 0.213 * s, "bg": 0.715 - 0.715 * s, "bb": 0.072 + 0.928 * s,
    }
    return ":".join(f"{k}={v:.4f}" for k, v in m.items())


def render(name, source, box, blur, sat):
    bw, bh = box
    w = max(1, round(SIGMA * bw / blur))
    h = max(1, round(w * bh / bw))
    sigma = blur * w / bw
    m = math.ceil(3 * sigma)  # three sigmas: the rest of the spread is below one 8-bit step
    chain = ",".join([
        f"scale={w}:{h}:force_original_aspect_ratio=increase:flags=area",
        f"crop={w}:{h}",
        "format=gbrp",
        f"colorchannelmixer={saturate(sat)}",
        "format=rgba",
        f"pad={w + 2 * m}:{h + 2 * m}:{m}:{m}:color=black@0",
        # Transparent black around opaque content is premultiplied colour already, so blurring
        # it and dividing by the blurred alpha is the browser's premultiplied blur.
        f"gblur=sigma={sigma:.3f}:steps=6",
        "unpremultiply=inplace=1",
        "format=rgba",
    ])
    out = MEDIA / name
    subprocess.run(
        ["ffmpeg", "-v", "error", "-y", "-i", str(MEDIA / source), "-vf", chain,
         "-frames:v", "1", "-map_metadata", "-1", "-fflags", "+bitexact", str(out)],
        check=True,
    )
    print(f"{name}: {w + 2 * m}x{h + 2 * m}, {out.stat().st_size} bytes;"
          f" --mx: {m / w:.4f}; --my: {m / h:.4f};")


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    only = None
    if argv[:1] == ["--only"]:
        only = argv[1:]
        unknown = sorted(set(only) - {g[0] for g in GLOWS})
        if not only or unknown:
            sys.exit(f"render-site-glows: --only wants names from {[g[0] for g in GLOWS]}, got {only}")
    elif argv:
        sys.exit("usage: render-site-glows.py [--only NAME ...]")
    for glow in GLOWS:
        if only is None or glow[0] in only:
            render(*glow)


if __name__ == "__main__":
    main()
