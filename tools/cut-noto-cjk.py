#!/usr/bin/env python3
"""Cut the shipped `pkg/appfont-cjk.ttf` fallback face from the Noto Sans CJK KR variable font.

    python3 tools/cut-noto-cjk.py path/to/NotoSansCJKkr-VF.ttf

This is the sibling of `tools/cut-inter.py`. Inter is the app's TYPEFACE; this is the app's
**fallback face** — the second link of `text.rs`'s chain, reached only for codepoints Inter does
not carry. `pkg/appfont.ttf` covers 2853 codepoints (Latin, Cyrillic, Greek) and **zero** Hangul,
Kana or Han, so before this face existed a Korean, Japanese or Chinese library rendered as 100%
tofu and nothing in any test tier could see it. `fontcov.rs`'s host gate is what stops that
returning; this script is what makes the gate satisfiable.

**The input is PINNED, by tag, path AND sha256** (the three constants below, asserted before
anything is read). "The current release" is not a reproducible dependency: Noto CJK re-cuts its
Han glyphs between releases, and a silent re-cut would move every ideograph's design without
moving a single line of code.

Four things are deliberately NOT done here, and each one is a difference from `cut-inter.py` that
a reader will otherwise assume is an omission:

  1. **No `opsz`.** Noto Sans CJK has one axis, `wght`. There is no optical-size choice to make.

  2. **No tabular-figure freeze.** `cut-inter.py` freezes `tnum` into cmap because the HUD clock
     measures a digit template and would otherwise breathe ~5 px. Digits never reach this face:
     U+0030..U+0039 are covered by Inter, so `text.rs`'s run splitter always resolves them to
     link 1. Freezing anything here would be freezing it for a code path that cannot execute.
     (The face's own Latin — which it does carry, in full — is dead for the same reason.)

  3. **No legacy `kern` synthesis.** Same argument, plus CJK is not kerned: Noto Sans CJK's GPOS
     carries `palt`/`vpal` proportional-alternate positioning, not Latin-style pair kerning, and
     SDL2_ttf 2.0.x reads neither (see `cut-inter.py` §3 — `FT_Get_Kerning` never reads GPOS).
     Latin runs are kerned by Inter's own `kern` table, on the Inter link.

  4. **No second weight.** The `wght` 700 instance is another ~13 MB in a package that is
     currently ~5 MB, which is not a trade this app can make for a fallback face. `text.rs` opens
     THIS face for bold runs too, without synthetic emboldening — see `text.rs::link_font` for why
     smearing a 20-stroke ideograph is worse than a weight mismatch beside it.

**What IS dropped, and why it is not "narrowing the subset".** The codepoint set is untouched —
all 44810 of them ship, with the cmap format-14 (UVS) subtable. What goes is, first, tables this
renderer provably cannot read:

  * `GSUB` (163 KB) and `GPOS` (78 KB) + `GDEF` — SDL2_ttf 2.0.x does no shaping and no OpenType
    feature selection at all (the device's is 2.0.14; HarfBuzz support landed in 2.0.18). Dropping
    them also makes the desktop simulator, whose SDL2_ttf may be newer, render the same thing the
    television does instead of quietly better.
  * `vhea` + `vmtx` (261 KB) — vertical writing metrics. Nothing in this app lays out vertically.
  * `BASE` (278 B), `DSIG` (8 B) — baseline tables for a shaper we do not have, and a signature
    that is invalid the moment the font is instanced.

Second, the glyphs no codepoint reaches. With GSUB gone, the ~21.4k alternates (vertical, locale,
ruby and width forms) are unreachable, and they are what makes the instance 65535 glyphs and its
long `loca` exactly 0x40000 bytes — which FreeType 2.4.11 through 2.7.0 (`ttpload.c`,
`table_len >= 0x40000L`) refuses to open (error 8), and several firmware inventories ship such a
FreeType. The fontTools subsetter keeps the 44,074 cmap-reachable glyphs (plus the 12-selector UVS
glyphs): 44,097 glyphs, identical outlines and advances, about 13.4 MB instead of 21.2 MB. The
script asserts the glyph count and the `loca` bound; `fontcov.rs` asserts them on the shipped file.

Name IDs 0, 7, 13 and 14 (copyright, trademark, licence, licence URL) are asserted present on the
way out: OFL 1.1 §2 requires the notice to travel with the Font Software, and `pkg/OFL.txt` alone
does not discharge it. Note the Reserved Font Name in this font's own notice is **'Source'** (Noto
CJK is built from Source Han Sans), not "Noto" — so OFL §3 imposes no rename and the family name
"Noto Sans CJK KR" is retained, exactly as `cut-inter.py` retains "Inter".
"""
import hashlib
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

# --- the pin. Change all four together or not at all. ------------------------------------------
UPSTREAM = "https://github.com/notofonts/noto-cjk"
RELEASE_TAG = "Sans2.004"
ASSET = "Sans/Variable/TTF/NotoSansCJKkr-VF.ttf"
ASSET_SHA256 = "7715af52f5fe77153ce5678546258993982d2da61abea8d25fb89eb5aaec5ca6"
ASSET_BYTES = 36140528
# The regional variant is `kr` ON PURPOSE. Every Noto CJK variant covers Han + Kana + Hangul in
# full; they differ only in which Han glyph FORM is the default. Korean-preferred is the right
# default for an LG submission. The `Subset/NotoSansKR-VF.ttf` in the same release is a different
# thing entirely — it is region-subsetted and DROPS the Han ideographs Korean does not need, which
# makes it wrong for a general fallback.

WGHT = 400
DROP_TABLES = ("GSUB", "GPOS", "GDEF", "BASE", "DSIG", "vhea", "vmtx")
# FreeType 2.4.11 .. 2.7.0 (`ttpload.c`: `table_len >= 0x40000L` -> Invalid_Table) cannot open a font
# whose long `loca` is 0x40000 bytes or more, i.e. 65535 glyphs. Several firmware inventories ship
# such a FreeType; the unsubsetted cut (65535 glyphs) failed there with error 8.
LOCA_LIMIT = 0x40000
LICENCE_NAME_IDS = {0: "copyright", 7: "trademark", 13: "licence", 14: "licence URL"}

# What the cut MUST still cover when it is done — a coarse self-check so a bad instancer upgrade
# fails here rather than in `fontcov.rs`'s gate an hour later. (start, end, minimum count).
COVERAGE_FLOOR = (
    ("Hangul syllables", 0xAC00, 0xD7A3, 11172),
    ("Hangul jamo", 0x1100, 0x11FF, 256),
    ("Hiragana", 0x3040, 0x309F, 90),
    ("Katakana", 0x30A0, 0x30FF, 90),
    ("CJK unified", 0x4E00, 0x9FFF, 20900),
    ("CJK ext-A", 0x3400, 0x4DBF, 6500),
    ("CJK symbols", 0x3000, 0x303F, 60),
)


def covered(font: TTFont) -> set:
    cps = set()
    for table in font["cmap"].tables:
        cps |= set(table.cmap.keys())
    return cps


def check_input(path: Path) -> None:
    if path.name != Path(ASSET).name:
        raise SystemExit(
            f"expected {Path(ASSET).name} (from {UPSTREAM} @ {RELEASE_TAG}, path {ASSET}), got {path.name}")
    blob = path.read_bytes()
    if len(blob) != ASSET_BYTES:
        raise SystemExit(f"{path.name}: {len(blob)} bytes, expected {ASSET_BYTES}")
    got = hashlib.sha256(blob).hexdigest()
    if got != ASSET_SHA256:
        raise SystemExit(
            f"{path.name}: sha256 {got}\n  expected {ASSET_SHA256}\n"
            f"  This is a PINNED input. If you meant to move to a new release, update RELEASE_TAG,\n"
            f"  ASSET, ASSET_SHA256 and ASSET_BYTES together, and re-read the coverage gate in\n"
            f"  rust-modules/base/src/fontcov.rs — a re-cut moves every ideograph's design.")


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__.strip().splitlines()[2].strip())
    src = Path(sys.argv[1])
    check_input(src)
    out = Path(__file__).resolve().parent.parent / "pkg" / "appfont-cjk.ttf"

    # recalcTimestamp=False, or the cut is NOT REPRODUCIBLE: fontTools stamps `head.modified` with
    # the current time on save, so two runs of this script over the same pinned input produce two
    # different sha256s. Measured — the first two runs here differed by exactly that field. The
    # whole point of pinning the input is that the output can be re-derived and compared; a clock
    # in the artifact destroys that, and it destroys it silently, which is the same shape as the
    # reproducibility work in `ci/mkipk.py`.
    font = TTFont(src, recalcTimestamp=False)
    before = covered(font)

    # A full pin on the only axis: `fvar`/`gvar`/`avar`/`HVAR`/`STAT` go with it. updateFontNames
    # rewrites the family/subfamily off STAT — without it the output keeps the variable font's
    # DEFAULT instance names, which are "NotoSansCJKkr-Thin" (the axis minimum), so the shipped
    # regular face would announce itself as Thin to every tool that reads a name table.
    instancer.instantiateVariableFont(font, {"wght": WGHT}, inplace=True, optimize=True,
                                      updateFontNames=True)
    for tag in DROP_TABLES:
        if tag in font:
            del font[tag]

    # Subset to the cmap-reachable glyphs. The GSUB table is already gone, so the ~21.4k glyphs the
    # instance carried beyond the 44,074 the cmap reaches (GSUB alternates: vertical forms, locale
    # forms, ruby, ...) are DEAD WEIGHT, and they are what pushes the glyph count to 65535 and the
    # long `loca` to exactly 0x40000. Every codepoint is kept (so `covered()` below must still be
    # equal), the format-14 UVS subtable stays, outlines/advances/hinting are untouched.
    opts = subset.Options()
    opts.layout_features = []
    opts.name_IDs = ["*"]
    opts.name_languages = ["*"]
    opts.name_legacy = True
    opts.notdef_outline = True
    opts.notdef_glyph = True
    opts.hinting = True
    opts.glyph_names = "post" in font and font["post"].formatType != 3.0
    opts.legacy_cmap = True
    opts.symbol_cmap = True
    opts.drop_tables = [t for t in opts.drop_tables if t not in ("cmap",)]
    sub = subset.Subsetter(options=opts)
    # The subsetter keeps a format-14 (UVS) subtable only for selectors it is ASKED to keep, and a
    # selector (U+FE00.., U+E0100..) is in no other subtable, so name them from the table itself.
    selectors = {vs for t in font["cmap"].tables if t.format == 14 for vs in t.uvsDict}
    if not selectors:
        raise SystemExit("the instance has no cmap format-14 (UVS) subtable to keep")
    sub.populate(unicodes=sorted(before | selectors))
    sub.subset(font)
    n_glyphs = len(font.getGlyphOrder())
    if n_glyphs > 65534:
        raise SystemExit(f"{n_glyphs} glyphs: FreeType <= 2.7.0 cannot open >= 65535")
    if font["head"].indexToLocFormat == 1:
        loca_bytes = (n_glyphs + 1) * 4
        if loca_bytes >= LOCA_LIMIT:
            raise SystemExit(f"long loca is {loca_bytes} bytes, FreeType <= 2.7.0 needs < {LOCA_LIMIT}")

    if not any(t.format == 14 for t in font["cmap"].tables):
        raise SystemExit("subsetting dropped the cmap format-14 (UVS) subtable")
    after = covered(font)
    if after != before:
        raise SystemExit(f"instancing changed coverage: {len(before)} -> {len(after)} codepoints")
    for label, lo, hi, floor in COVERAGE_FLOOR:
        n = sum(1 for cp in range(lo, hi + 1) if cp in after)
        if n < floor:
            raise SystemExit(f"{label}: {n} codepoints, expected at least {floor}")
    have = {r.nameID for r in font["name"].names}
    missing = [why for nid, why in LICENCE_NAME_IDS.items() if nid not in have]
    if missing:
        raise SystemExit(f"name table lost the OFL notice chain: {', '.join(missing)}")

    out.parent.mkdir(parents=True, exist_ok=True)
    font.save(out)
    # The OUTPUT hash, printed rather than asserted. Two runs of one fontTools version over the
    # pinned input now agree byte for byte (see recalcTimestamp above), so this is comparable — but
    # a fontTools upgrade may legitimately reorder or repad tables without changing the font, so
    # pinning it here would fail for the wrong reason. What IS asserted is the semantics:
    # the coverage floors above, and `fontcov.rs`'s exact 44810.
    print(f"{out}  {out.stat().st_size:,} bytes  {len(after):,} codepoints  {n_glyphs:,} glyphs")
    print(f"  sha256   {hashlib.sha256(out.read_bytes()).hexdigest()}")
    print(f"  family   {font['name'].getDebugName(1)!r} / {font['name'].getDebugName(2)!r}")
    print(f"  from     {UPSTREAM} @ {RELEASE_TAG}  {ASSET}")
    print(f"  dropped  {', '.join(DROP_TABLES)}")


if __name__ == "__main__":
    main()
