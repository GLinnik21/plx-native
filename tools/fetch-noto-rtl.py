#!/usr/bin/env python3
"""Fetch and verify the shipped right-to-left faces: `pkg/appfont-arabic.ttf`,
`pkg/appfont-arabic-bold.ttf`, `pkg/appfont-hebrew.ttf` and `pkg/appfont-hebrew-bold.ttf`.

    python3 tools/fetch-noto-rtl.py [DIR]

DIR holds the four pinned inputs below; without it they are downloaded from their pinned URLs.

The Arabic and Hebrew links of `text.rs`'s fallback chain. Unlike every other link they are never
opened by SDL2_ttf: their runs are shaped and rasterized by the bundled libass-plx's UI text facade
(`src/ass_text.c`, `rust-modules/gfx/src/rtl.rs`), which is why, unlike its `cut-noto-*` siblings,
this tool cuts NOTHING. Each face ships byte for byte as the Noto project publishes it:

* `GSUB` (Arabic contextual forms, the lam-alef ligature, Persian and Urdu letter forms), `GPOS`
  (mark anchors, kerning) and `GDEF` are what the shaper reads. `cut-noto-emoji.py` drops them
  because SDL2_ttf cannot use them; here they are the reason the faces work at all.
* Noto Kufi Arabic is drawn to sit beside Noto Sans at the same size, and Inter is within 2 % of
  Noto Sans (capitals 0.728 em against 0.714 em), so it is used at its designed size. Scaled
  variants (alef matched to Inter's capitals, 0.96; and 0.92) were rendered beside it through this
  same facade and not chosen.
* Each script ships a real Bold: a bold label's Arabic beside bold Inter must not be the regular
  weight, and synthetic emboldening fills a cursive face's counters.

**The inputs are PINNED, by URL, size AND sha256**: the unhinted static TTFs of the Noto project's
own distribution repository (`notofonts/notofonts.github.io`) at one commit, Noto Kufi Arabic 2.110
and Noto Sans Hebrew 3.001. Unhinted because the facade hints with FreeType's light auto-hinter,
like every other face in the app. Both are OFL 1.1 with no Reserved Font Name; name IDs 0, 13 and
14 (copyright, licence, licence URL) are asserted, as in `cut-noto-cjk.py`.
"""
import hashlib
import shutil
import sys
import tempfile
import urllib.request
from pathlib import Path

from fontTools.ttLib import TTFont

# --- the pin. Change a row's path, size and sha256 together, with COMMIT, or not at all. ----------
REPO = "https://raw.githubusercontent.com/notofonts/notofonts.github.io"
COMMIT = "5c3913063bb189df4edc94b766584dfb4c04a187"
# shipped name -> (upstream path under fonts/, bytes, sha256)
SOURCES = {
    "appfont-arabic.ttf": ("NotoKufiArabic/unhinted/ttf/NotoKufiArabic-Regular.ttf", 139236,
                           "e245bdc293b07d903603054f88746e8e51d9ff0bfd88905b0d9842198732d2b1"),
    "appfont-arabic-bold.ttf": ("NotoKufiArabic/unhinted/ttf/NotoKufiArabic-Bold.ttf", 138700,
                                "a2688019b048aedbc6a232d4332de5a02d938432e356ed694d62de0ae0d6fa1d"),
    "appfont-hebrew.ttf": ("NotoSansHebrew/unhinted/ttf/NotoSansHebrew-Regular.ttf", 16836,
                           "04272f5600d0ec816d31d0df73b23aa8d3501ea359ebe820da31c11ffcf00853"),
    "appfont-hebrew-bold.ttf": ("NotoSansHebrew/unhinted/ttf/NotoSansHebrew-Bold.ttf", 16900,
                                "dfdb3056de1f4542b888c77a1a8a750548a802e271479f56e52152423b64dde8"),
}

LICENCE_NAME_IDS = {0: "copyright", 13: "licence", 14: "licence URL"}
SHAPING_TABLES = ("GSUB", "GPOS", "GDEF")

# A coarse self-check, so a wrong file fails here rather than in `fontcov.rs`'s gate.
MUST_COVER = {
    "arabic": "ابتثجحخدذرزسشصضطظعغفقكلمنهوي ءآأؤإئة ٠١٢٣٤٥٦٧٨٩ ،؛؟ پچژگکی",
    "hebrew": "אבגדהוזחטיכלמנסעפצקרשת ךםןףץ ־",
}


def url(path: str) -> str:
    return f"{REPO}/{COMMIT}/fonts/{path}"


def check(path: Path, size: int, sha: str, script: str) -> None:
    blob = path.read_bytes()
    if len(blob) != size:
        raise SystemExit(f"{path.name}: {len(blob)} bytes, expected {size}")
    got = hashlib.sha256(blob).hexdigest()
    if got != sha:
        raise SystemExit(
            f"{path.name}: sha256 {got}\n  expected {sha}\n"
            f"  This is a PINNED input. To move it, update COMMIT and every row of SOURCES together,\n"
            f"  and re-run `make check` (fontcov.rs pins the faces' coverage).")
    font = TTFont(path)
    cmap = font.getBestCmap()
    missing = [c for c in MUST_COVER[script] if c != " " and ord(c) not in cmap]
    if missing:
        raise SystemExit(f"{path.name} does not map {missing}")
    lost = [t for t in SHAPING_TABLES if t not in font]
    if lost:
        raise SystemExit(f"{path.name} has no {lost}: nothing to shape with")
    have = {r.nameID for r in font["name"].names}
    gone = [why for nid, why in LICENCE_NAME_IDS.items() if nid not in have]
    if gone:
        raise SystemExit(f"{path.name}: the name table has no OFL notice chain: {', '.join(gone)}")


def main() -> None:
    if len(sys.argv) > 2:
        raise SystemExit("usage: python3 tools/fetch-noto-rtl.py [DIR]")
    pkg = Path(__file__).resolve().parent.parent / "pkg"
    with tempfile.TemporaryDirectory() as tmp:
        src_dir = Path(sys.argv[1]) if len(sys.argv) == 2 else Path(tmp)
        for shipped, (path, size, sha) in SOURCES.items():
            src = src_dir / Path(path).name
            if len(sys.argv) == 1:
                print(f"fetch {url(path)}")
                with urllib.request.urlopen(url(path), timeout=60) as r:
                    src.write_bytes(r.read())
            check(src, size, sha, "arabic" if shipped.startswith("appfont-arabic") else "hebrew")
            shutil.copyfile(src, pkg / shipped)
            font = TTFont(pkg / shipped)
            print(f"{pkg / shipped}  {size:,} bytes  {len(font.getBestCmap()):,} codepoints  "
                  f"{font['name'].getDebugName(4)!r} {font['name'].getDebugName(5)!r}")
            print(f"  sha256   {sha}")


if __name__ == "__main__":
    main()
