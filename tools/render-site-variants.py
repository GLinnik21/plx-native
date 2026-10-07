#!/usr/bin/env python3
"""Derive the landing page's WebP and phone copies of the close-ups from the rendered JPEGs.

    python3 tools/render-site-variants.py

`make screenshots` renders each close-up once, as a JPEG at the size its scene names
(`tests/screenshots/scenes.json`, the `site-*` scenes). The page serves more than that: a WebP
of every close-up (its <picture> prefers it; the JPEG is the fallback), and a smaller phone copy
(`-narrow`, the `max-width: 759px` source) of the tiles and the player close-ups. The glass
close-up's phone copy is a different crop, so it is a scene of its own and only gets its WebP
here. Before this script those copies were cut by hand once, and the next re-render left them
behind at the old resolution.

Every output is derived from the rendered JPEG beside it, never from a previous output, so a
second run writes the same bytes. Run it after `make screenshots` and before
`tools/render-site-glows.py` (which only reads the JPEGs). Needs an ffmpeg with libwebp.
"""
import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parent.parent
MEDIA = ROOT / "site" / "media"

# The rendered close-ups, each gets a WebP of the same size.
RENDERED = ["closeup-glass.jpg", "closeup-glass-narrow.jpg", "closeup-tiles.jpg", "closeup-player.jpg"]
# source, phone copy, its width. On a phone the two cards share one swipeable row, each 86% of
# the window (styles.css, `.closeup-row`): 1200 px covers a 430 CSS px window at DPR 3 for the
# player (shown whole) and at DPR 2.5 for the tiles (cropped to the card's 1900:1540 shape).
NARROW = [
    ("closeup-tiles.jpg", "closeup-tiles-narrow", 1200),
    ("closeup-player.jpg", "closeup-player-narrow", 1200),
]
# libwebp quality, and ffmpeg's -q:v for the phone JPEG (the scenes' own default is 2).
WEBP_QUALITY = 82
JPEG_QSCALE = 3


def ffmpeg(src, dst, vf, *codec):
    args = ["ffmpeg", "-v", "error", "-y", "-i", str(src)]
    if vf:
        args += ["-vf", vf]
    args += ["-frames:v", "1", "-map_metadata", "-1", "-fflags", "+bitexact", "-threads", "1",
             *codec, str(dst)]
    subprocess.run(args, check=True)
    print(f"{dst.name}: {dst.stat().st_size} bytes")


def webp(src, dst, vf=None):
    ffmpeg(src, dst, vf, "-c:v", "libwebp", "-quality", str(WEBP_QUALITY),
           "-compression_level", "6", "-preset", "picture")


def main():
    for name in RENDERED:
        src = MEDIA / name
        webp(src, src.with_suffix(".webp"))
    for name, stem, width in NARROW:
        src = MEDIA / name
        vf = f"scale={width}:-2:flags=lanczos"
        ffmpeg(src, MEDIA / f"{stem}.jpg", vf, "-q:v", str(JPEG_QSCALE))
        webp(src, MEDIA / f"{stem}.webp", vf)


if __name__ == "__main__":
    main()
