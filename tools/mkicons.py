#!/usr/bin/env python3
"""Cut the shipped app icons from a single square logo master.

    python3 tools/mkicons.py assets/logo-master.png [--band=N] [--splash=assets/splash-master.png]
    python3 tools/mkicons.py assets/logo-master.png --out-dir=pkg/dev --sizes=80,130 --badge=DEV

Emits `pkg/icon.png` (80), `pkg/largeIcon.png` (130) and, for the webosbrew channel listing,
`pkg/icon160.png` / `pkg/icon320.png`. With `--splash` it also emits `pkg/splash.png` at exactly
1920x1080, the size `splashBackground` requires and the size all four native-app splashes on the
dev TV actually are (Amazon, Apple TV, Netflix, YouTube — checked, since the field is documented
in a web-app context and it was not obvious it applies to `type: "native"` at all; it does).
Re-running this is the only supported way to change any of them.

**`--badge=TEXT` cuts the set for a SECOND INSTALL** — the developer build that lives beside the
released app on the same television (`com.beb.plxnative.debug`; the Makefile's FLAVOR block is the
account). The tiles sit side by side in the launcher, so the badge's whole job is to be unmistakable
at the smallest size webOS ever draws.

It is a full-bleed BOTTOM BAR, and both halves of that are load-bearing rather than taste:

  * **Bottom, not a corner ribbon or dot.** `appinfo.json`'s `iconColor` paints the launcher tile
    *behind* the icon, and `ci/check-package.py` compares it against the icon's own corner pixel —
    a gate that exists because a gold tile shipped under a black icon until 2026-08-02 and was
    invisible in every file, since it only exists once the system composites. A corner mark changes
    pixel (1,1) and would fail it by ~240 levels; a bottom bar leaves the corner alone, so ONE
    `iconColor` stays correct for both flavours and the badge needs no descriptor change at all.
  * **A bar, not a whole-tile tint.** Tinting the tile means moving `iconColor` in lockstep or
    reproducing exactly the defect above — and it stops looking like the product.

The colours are the app's own `theme::RESUME_FILL` amber over `AMBER_950` ink, i.e. a pair the
design system already uses on a filled control, so the badge is on-brand while being the one thing
on the tile that could not be mistaken for the release artwork. The text colour is DERIVED from the
fill by relative luminance rather than fixed, so a different `--badge-fill` cannot silently produce
grey-on-grey.

The bar is rasterized NATIVELY at each output size (never scaled from one master), because a 3-px
stroke scaled 4x down is a smear where a 3-px stroke drawn at 80 is a stroke.

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
from PIL import Image, ImageDraw, ImageFont

# Fraction of the tile the logo's ink should span horizontally. Mid-pack against the four store
# icons measured above; also keeps >=5px padding at 130 (130*0.78 -> 101 wide, 14 px each side).
INK_FRACTION = 0.78
# (filename, size). 80/130 are LG's launcher pair; 160/320 are the webosbrew channel's
# iconUri/detailIconUri; 400 is the LG Content Store listing icon, whose uploader refuses anything
# under 400x400 ("Upload 400 x 400 pixels and greater icons only"). Only the first two are staged
# into the ipk (the Makefile's ICONS/APP_FILES name them explicitly) — the rest are listing assets.
TARGETS = (("icon.png", 80), ("largeIcon.png", 130), ("icon160.png", 160), ("icon320.png", 320),
           ("icon400.png", 400))

# --- the flavour badge -------------------------------------------------------------------------
# `theme::RESUME_FILL` (AMBER_300) over `AMBER_950` — the design system's own filled-control pair.
BADGE_FILL = "#fab82e"
# Bar height as a fraction of the tile. 22% is 18 px at the 80 px launcher size, which leaves room
# for a ~10 px cap height — comfortably above the ~8 px floor where a glyph stops being a glyph
# (the same threshold `--band`'s advice is built on).
BADGE_BAR = 0.22
# Cap height as a fraction of the bar, leaving ~22% of the bar as padding above and below.
BADGE_CAP = 0.55
# The bold face the app itself renders in. Deliberately NOT `ImageFont.load_default()`, which is an
# unscalable 10 px bitmap on older Pillow and a differently-metricked fallback on newer — either
# way it makes the badge's size depend on which machine cut it.
BADGE_FONT = "pkg/appfont-bold.ttf"


def badge_ink(fill: str) -> str:
    """Black or white over `fill`, whichever has more contrast. Derived, never assumed.

    A fixed ink colour is one `--badge-fill` away from grey-on-grey, and the failure would be a
    tile nobody can read rather than an error. sRGB relative luminance, WCAG's formula.
    """
    def lin(c: float) -> float:
        return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4
    r, g, b = (int(fill.lstrip("#")[i:i + 2], 16) / 255 for i in (0, 2, 4))
    lum = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    # Contrast against white is (1.05)/(L+0.05); against black it is (L+0.05)/0.05.
    return "#1a1204" if (lum + 0.05) / 0.05 > 1.05 / (lum + 0.05) else "#f7fafc"


def draw_badge(tile: Image.Image, text: str, fill: str, repo: Path, bar: int) -> None:
    """Stamp a full-bleed bottom bar of height `bar` carrying `text`.

    The height comes from the CALLER, which already solved it for the wordmark lift — one
    derivation, so the drawn bar and the lift cannot move independently.

    Drawn at the tile's OWN size — never scaled from a larger master — because at 80 px the bar is
    18 px tall and the glyphs about 10, and a 4x downsample of either is a smear.
    """
    n = tile.size[0]
    cap = max(1, round(bar * BADGE_CAP))
    face = repo / BADGE_FONT
    if not face.exists():
        raise SystemExit(f"{BADGE_FONT} is missing — the badge needs the app's own bold face "
                         f"(Pillow's default is an unscalable bitmap and would size differently "
                         f"on every machine)")
    d = ImageDraw.Draw(tile)
    d.rectangle([0, n - bar, n, n], fill=fill)
    # Size the face so the CAP height lands on `cap`, measured on the text actually being drawn
    # rather than on the font's nominal size — which is em-relative and differs between faces.
    size = cap
    for _ in range(12):
        f = ImageFont.truetype(str(face), size)
        bbox = d.textbbox((0, 0), text, font=f)
        h = bbox[3] - bbox[1]
        if h <= 0:
            break
        adj = round(size * cap / h)
        if adj == size:
            break
        size = max(1, adj)
    f = ImageFont.truetype(str(face), size)
    x0, y0, x1, y1 = d.textbbox((0, 0), text, font=f)
    # Snap to whole pixels: a fractional origin under any resampling smears a 2-px stem, which is
    # the same contract `gfx::snap` enforces for 1:1-texel content in the app itself.
    tx = round((n - (x1 - x0)) / 2) - x0
    ty = round(n - bar + (bar - (y1 - y0)) / 2) - y0
    d.text((tx, ty), text, font=f, fill=badge_ink(fill))


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


def write_splash(src: Path, out: Path, lift=None) -> None:
    """Emit the 1920x1080 splash. LANCZOS because the art is vector-derived with hard edges.

    `lift` raises the BLACK POINT to a hex colour, remapping [0,255] -> [lift,255] per channel.
    It exists for one line in LG's App Resources page — *"The splash screen should not be black
    and should use minimal text to avoid localization issues."* The master measures mean RGB
    (23,15,10) with 63% of its pixels effectively black, which is exactly what that sentence warns
    against, and a QA reader who takes it literally is the whole audience for this artwork.

    Lift rather than brighten: a linear remap keeps the logo's highlights at 255 and only opens up
    the field behind it, so the mark does not go grey. Passing the app's own `theme::SURFACE_APP`
    (#2C2C2E) additionally makes the splash match the FIRST FRAME the app draws, so boot stops
    stepping from near-black to the shelf gray.
    """
    im = Image.open(src).convert("RGB")
    if lift:
        f = [int(lift.lstrip("#")[i:i + 2], 16) for i in (0, 2, 4)]
        a = np.asarray(im).astype(float)
        for c in range(3):
            a[..., c] = f[c] + a[..., c] * (255.0 - f[c]) / 255.0
        im = Image.fromarray(a.round().clip(0, 255).astype("uint8"), "RGB")
        print(f"  splash black point lifted to #{lift.lstrip('#').upper()}")
    if im.size != (1920, 1080):
        w, h = im.size
        if abs(w / h - 16 / 9) > 0.01:
            raise SystemExit(f"splash master is {w}x{h} — not 16:9, so fitting it would letterbox "
                             f"or crop the artwork. Re-export it at 1920x1080.")
        print(f"  splash master is {w}x{h}; resampling to 1920x1080 ({1920 / w:.3f}x)")
        im = im.resize((1920, 1080), Image.LANCZOS)
    im.save(out, optimize=True)
    print(f"  {out.name:14s} 1920x1080")


#: Every option this script accepts. UNKNOWN FLAGS ARE AN ERROR, and that is not pedantry: the old
#: parser filtered out everything starting with `--` and read the four it knew, so a typo
#: (`--outdir=pkg/dev`) silently wrote the BADGED set over `pkg/icon.png`, `pkg/largeIcon.png` and
#: `pkg/icon160.png` — the last of which release.yml publishes as a raw.githubusercontent URL for
#: the webosbrew channel listing. One mistyped letter, and the artwork thousands of people see
#: before installing carries a DEV bar.
OPTIONS = ("--band=", "--splash=", "--splash-lift=", "--out-dir=", "--sizes=", "--badge=", "--badge-fill=", "--appinfo=")


def opt(name: str, default=None):
    return next((a.split("=", 1)[1] for a in sys.argv[1:] if a.startswith(name)), default)


def main() -> int:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    bad = [a for a in sys.argv[1:] if a.startswith("--") and not a.startswith(OPTIONS)]
    if bad:
        raise SystemExit(f"unknown option(s) {' '.join(bad)} — accepts: {' '.join(OPTIONS)}")
    band_arg = opt("--band=")
    splash = opt("--splash=")
    badge = opt("--badge=")
    badge_fill = opt("--badge-fill=", BADGE_FILL)
    if len(args) != 1:
        return print(__doc__) or 2

    repo = Path(__file__).resolve().parent.parent
    # WHERE the set is written. A directory rather than a filename prefix, because the packaged
    # basenames are fixed: `appinfo.json`'s `icon`/`largeIcon` fields name `icon.png` and
    # `largeIcon.png`, and `ci/check-package.py` grades the ipk payload by BASENAME — so a
    # `dev-icon.png` would either have to be renamed during staging or drag a per-flavour
    # descriptor behind it. The flavour lives in the source path and nowhere else.
    out_dir = repo / opt("--out-dir=", "pkg")
    out_dir.mkdir(parents=True, exist_ok=True)
    # Which sizes. 80 and 130 are the launcher's; 160/320 exist only for the webosbrew channel
    # listing, which a second install is never in — so `--sizes=80,130` for a flavour.
    want = opt("--sizes=")
    targets = TARGETS if want is None else tuple(t for t in TARGETS if str(t[1]) in want.split(","))
    if not targets:
        raise SystemExit(f"--sizes={want} selects none of {[n for _, n in TARGETS]}")
    if splash:
        # THE SPLASH IS SHARED, so it is pinned to `pkg/` and never follows `--out-dir`. The
        # Makefile's `APP_FILES` stages `pkg/splash.png` unconditionally for every flavour — only
        # the descriptor and the icons are flavour-dependent — and `docs/two-installs.md` lists it
        # among the resources two installs share. Written into `pkg/dev/` it was a file nothing
        # would ever ship, while the operator watched it appear and believed the debug install's
        # splash had changed. Refusing is better than writing a decoy.
        if out_dir != repo / "pkg":
            raise SystemExit(
                "--splash cannot be combined with --out-dir: the splash is SHARED by every "
                "flavour (the Makefile stages pkg/splash.png for all of them), so a per-flavour "
                "copy would never be packaged. Run --splash on its own."
            )
        write_splash(Path(splash), out_dir / "splash.png", opt("--splash-lift="))
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

    # The descriptor to keep in step, if there is one beside the output. SKIPPED WITH A LINE rather
    # than falling back to `pkg/appinfo.json`: `sync_icon_color` writes unconditionally, so a
    # fallback would push a flavour's tile colour into the RELEASED descriptor — the exact
    # hard-edged-rectangle defect this function exists to prevent, inflicted on the other flavour.
    # (A flavoured descriptor is derived at package time by ci/flavor.py and is not on disk here,
    # which is why "absent" is the normal case rather than a mistake.)
    appinfo = Path(opt("--appinfo=", out_dir / "appinfo.json"))
    if appinfo.exists():
        was = sync_icon_color(appinfo, hexcolor)
        print(f"  {appinfo.name} iconColor {was} -> {hexcolor}"
              + ("   (unchanged)" if was.lower() == hexcolor else "   ** CHANGED **"))
    else:
        print(f"  no {appinfo} beside the output — iconColor not synced; it must already be {hexcolor}")

    if badge:
        print(f"  badge {badge!r} on {badge_fill} / {badge_ink(badge_fill)}"
              f"  (bar {BADGE_BAR:.0%} of the tile, cap {BADGE_CAP:.0%} of the bar)")

    for name, n in targets:
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
        # A badge takes the bottom of the tile, so the wordmark is lifted by HALF the bar to stay
        # optically centred in what is left. Half, not the whole bar: the mark then sits centred in
        # the visible field above the bar, which is what the eye reads as centred.
        bar = max(1, round(n * BADGE_BAR)) if badge else 0
        tile = Image.new("RGB", (n, n), tuple(bg))
        tile.paste(scaled, (round(-left), round(-upper) - bar // 2))
        if badge:
            # `bar` is already solved above for the wordmark lift; pass it rather than
            # letting `draw_badge` re-derive it, or the drawn bar and the lift can move
            # independently when BADGE_BAR's rounding changes.
            draw_badge(tile, badge, badge_fill, repo, bar)
        out = out_dir / name
        tile.save(out, optimize=True)
        w, h = round(ink_w * scale), round(ink_h * scale)
        # Padding is graded against whatever actually bounds the mark. With a badge that is the gap
        # to the BAR, not to the tile edge — the old expression would have reported LG's >=5px rule
        # satisfied while the wordmark sat directly on the amber.
        if badge:
            top_of_mark = round((n - bar) / 2) - h // 2
            pad = min((n - w) // 2, top_of_mark, (n - bar) - (top_of_mark + h))
        else:
            pad = min((n - w) // 2, (n - h) // 2)
        flag = "" if pad >= 5 else "   ** under LG's 5px minimum padding **"
        print(f"  {out.relative_to(repo)!s:24s} {n}x{n}  logo {w}x{h}"
              f"{f'  bar {bar}px' if badge else ''}  padding {pad}px{flag}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
