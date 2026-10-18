#!/usr/bin/env python3
"""Cut the shipped `pkg/appfont-emoji.ttf` fallback face from the monochrome Noto Emoji.

    python3 tools/cut-noto-emoji.py path/to/noto-emoji-emoji-400-normal.woff

The sibling of `tools/cut-noto-cjk.py`, for the link of `text.rs`'s fallback chain that draws
emoji, so a library named "Movies 🎬" or "Kids 🧸" draws its emoji.

**Why monochrome.** The television's text renderer is its own SDL2_ttf (2.0.10 or 2.0.14 on every
firmware `tools/fwcompat.py` knows), which draws an outline face into an alpha mask that
`fs_text.frag` tints. Colour emoji fonts (CBDT, sbix, COLR) need SDL2_ttf 2.0.18 or later. Noto
Emoji's monochrome outlines draw like any other glyph, in the label's own colour.

**Why every astral emoji is ALSO mapped into the Private Use Area.** SDL2_ttf before 2.0.18 keys
glyphs on a 16-bit character, so it cannot draw anything above U+FFFF. Most emoji (🎬 U+1F3AC,
🧸 U+1F9F8) are up there. So the cut gives the i-th astral codepoint of the font, in ascending
order, a second cmap entry at U+E000 + i that points at the same glyph. `text.rs` rewrites an
emoji run through `fontcov::Coverage::astral_rank` before it renders, and that rank is computed
from this file's own cmap, so the mapping cannot drift from the font: there is no table to keep
in sync.

**The input is PINNED, by package, path AND sha256.** Noto Emoji's upstream (`google/fonts`,
`ofl/notoemoji/NotoEmoji[wght].ttf`) is a variable font. The pinned file is Fontsource's static
wght-400 build of the same family, whose `emoji` subset carries every codepoint the font maps
(the other Fontsource subsets hold only the glyphs that ZWJ sequences reach through GSUB, which
a renderer with no shaper cannot use). The npm tarball is
`https://registry.npmjs.org/@fontsource/noto-emoji/-/noto-emoji-5.3.2.tgz`.

**What is dropped.** `GSUB`, `GPOS` and `GDEF` (ZWJ ligatures and skin-tone substitutions, which
SDL2_ttf 2.0.x never reads), `vhea`/`vmtx` (vertical metrics), `STAT` and `DSIG`. Every codepoint mapped in the input still ships.

Noto Emoji's licence is OFL 1.1 with no Reserved Font Name, so the family name is kept. The input's
name table carries the copyright and the licence URL but not the licence description (ID 13), so
the cut writes that entry. IDs 0, 13 and 14 are asserted on the way out, as in `cut-noto-cjk.py`.
"""
import hashlib
import sys
from pathlib import Path

from fontTools.ttLib import TTFont
from fontTools.ttLib.tables._c_m_a_p import CmapSubtable

# --- the pin. Change all four together or not at all. ------------------------------------------
UPSTREAM = "https://www.npmjs.com/package/@fontsource/noto-emoji"
RELEASE = "5.3.2"
ASSET = "package/files/noto-emoji-emoji-400-normal.woff"
ASSET_SHA256 = "61c7949ebb9d401f397826415e64525f60298bf2a9c469f9720aa17817877331"
ASSET_BYTES = 568256

PUA_BASE = 0xE000
PUA_END = 0xF8FF
DROP_TABLES = ("GSUB", "GPOS", "GDEF", "vhea", "vmtx", "STAT", "DSIG")
LICENCE_NAME_IDS = {0: "copyright", 13: "licence", 14: "licence URL"}
OFL_DESCRIPTION = ("This Font Software is licensed under the SIL Open Font License, Version 1.1. "
                   "This license is available with a FAQ at: https://openfontlicense.org")

# A coarse self-check, so a bad input fails here rather than in `fontcov.rs`'s gate.
MUST_COVER = (0x1F3AC, 0x1F9F8, 0x1F476, 0x1F9D2, 0x2B50, 0x2764, 0x1F600)


def covered(font: TTFont) -> set:
    cps = set()
    for table in font["cmap"].tables:
        cps |= set(table.cmap.keys())
    return cps


def check_input(path: Path) -> None:
    if path.name != Path(ASSET).name:
        raise SystemExit(f"expected {Path(ASSET).name} (from {UPSTREAM} @ {RELEASE}), got {path.name}")
    blob = path.read_bytes()
    if len(blob) != ASSET_BYTES:
        raise SystemExit(f"{path.name}: {len(blob)} bytes, expected {ASSET_BYTES}")
    got = hashlib.sha256(blob).hexdigest()
    if got != ASSET_SHA256:
        raise SystemExit(
            f"{path.name}: sha256 {got}\n  expected {ASSET_SHA256}\n"
            f"  This is a PINNED input. To move to a new release, update RELEASE, ASSET,\n"
            f"  ASSET_SHA256 and ASSET_BYTES together, and re-run `make check` (fontcov.rs).")


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__.strip().splitlines()[2].strip())
    src = Path(sys.argv[1])
    check_input(src)
    out = Path(__file__).resolve().parent.parent / "pkg" / "appfont-emoji.ttf"

    # recalcTimestamp=False keeps the cut reproducible (see cut-noto-cjk.py).
    font = TTFont(src, recalcTimestamp=False)
    font.flavor = None  # WOFF in, plain sfnt out: TTF_OpenFont reads sfnt only
    cmap = font.getBestCmap()
    before = set(cmap)
    for cp in MUST_COVER:
        if cp not in before:
            raise SystemExit(f"input does not map U+{cp:04X}")

    for tag in DROP_TABLES:
        if tag in font:
            del font[tag]

    astral = sorted(cp for cp in before if cp > 0xFFFF)
    if PUA_BASE + len(astral) - 1 > PUA_END:
        raise SystemExit(f"{len(astral)} astral codepoints do not fit the Private Use Area")
    if any(PUA_BASE <= cp <= PUA_END for cp in before):
        raise SystemExit("input already maps Private Use Area codepoints")
    full = dict(cmap)
    for i, cp in enumerate(astral):
        full[PUA_BASE + i] = cmap[cp]

    # One BMP subtable (format 4, both Unicode platforms) and one full one (format 12). SDL2_ttf
    # selects a Unicode charmap through FreeType, which prefers the format 12 table when present.
    bmp = {cp: g for cp, g in full.items() if cp <= 0xFFFF}
    tables = []
    for platform, enc, fmt, mapping in ((0, 3, 4, bmp), (0, 4, 12, full), (3, 1, 4, bmp), (3, 10, 12, full)):
        t = CmapSubtable.newSubtable(fmt)
        t.platformID, t.platEncID, t.language = platform, enc, 0
        t.cmap = dict(mapping)
        tables.append(t)
    font["cmap"].tables = tables

    name = font["name"]
    name.setName(OFL_DESCRIPTION, 13, 3, 1, 0x409)
    have = {r.nameID for r in name.names}
    missing = [why for nid, why in LICENCE_NAME_IDS.items() if nid not in have]
    if missing:
        raise SystemExit(f"name table lost the OFL notice chain: {', '.join(missing)}")

    after = covered(font)
    if after != before | {PUA_BASE + i for i in range(len(astral))}:
        raise SystemExit("the cut changed the emoji coverage")

    out.parent.mkdir(parents=True, exist_ok=True)
    font.save(out)
    print(f"{out}  {out.stat().st_size:,} bytes  {len(before):,} codepoints "
          f"({len(astral):,} astral, mirrored at U+{PUA_BASE:04X}..U+{PUA_BASE + len(astral) - 1:04X})")
    print(f"  sha256   {hashlib.sha256(out.read_bytes()).hexdigest()}")
    print(f"  family   {name.getDebugName(1)!r} / {name.getDebugName(2)!r}")
    print(f"  from     {UPSTREAM} @ {RELEASE}  {ASSET}")
    print(f"  dropped  {', '.join(DROP_TABLES)}")


if __name__ == "__main__":
    main()
