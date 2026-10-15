#!/usr/bin/env python3
"""Cut the shipped subtitle faces `pkg/fonts/subs/*.ttf` (and their manifest) from pinned Noto Sans
Bold releases, and keep the "PlxSubs" alias on `pkg/appfont-bold.ttf`.

    python3 tools/cut-noto-subs.py --fetch DIR      # download the pinned inputs into a NEW, empty DIR
    python3 tools/cut-noto-subs.py DIR              # cut from DIR (verifies every sha256 first)
    python3 tools/cut-noto-subs.py --fetch DIR --cut   # both
    python3 tools/cut-noto-subs.py --check          # re-verify the committed faces against the manifest

This is the sibling of `tools/cut-inter.py` (the UI face) and `tools/cut-noto-cjk.py` (the UI's CJK
fallback). Those two feed SDL2_ttf, which does no shaping. THESE faces feed libass, which shapes
with HarfBuzz, so the cut is the opposite of `cut-noto-cjk.py`'s on the one point that matters:
**GSUB, GPOS and GDEF are KEPT** (and every feature of every script), or Thai, Devanagari and
Arabic would draw unshaped. See `docs/ass-subtitles.md` "Fonts" for how libass picks a face.

**What ships, and why (plan D6).** libass with `ASS_FONTPROVIDER_NONE` selects: the authored family
(with a per-glyph coverage check), then `family_default` ("Inter"), then `path_default` (the CJK
file, with NO coverage check). Thai, Devanagari, Arabic and the rest must therefore be reachable
BY FAMILY NAME. Each face here keeps its real English name ("Noto Sans Thai", name ID 1 under
Windows langID 0x0409) and gains "PlxSubs" (0x0407) and "Inter" (0x040C) as ID 1 and ID 4 records
(`tools/plxsubs_names.py` says why those languages). `pkg/appfont-bold.ttf` gets "PlxSubs" too, and
is added to the renderer FIRST, so Latin, digits and punctuation never fall to a Noto face.

**Which scripts.** Every script used by a language in the account language picker
(`rust-modules/plex/src/plex/languages.rs`) for which Noto Sans ships a Bold weight; the list is
`FACES` below, with each face's languages recorded in the manifest. A script outside it has no face
and falls to `path_default`; `UNCOVERED` names those, with the reason, so the gap is documented and
a test (`ass/fonts/tests.rs`) holds it. Tibetan is the one face not from the current release repository:
`notofonts.github.io` ships no Noto Sans Tibetan, so it is the archived `noto-fonts` file.

**The cut, per face.**
  1. Subset to the upstream cmap MINUS the codepoints Inter Bold already covers and MINUS
     U+0000-024F, U+0300-036F, U+2000-206F and U+20A0-20CF, so a Noto face never competes with
     Inter for Latin, digits and punctuation (the add order already makes Inter win a tie; this
     removes the dead weight and the ambiguity). KEPT regardless of those ranges, when upstream has
     them: U+200C/U+200D (joiners) and U+25CC (the dotted circle HarfBuzz inserts for a broken
     cluster, looked up in the face being shaped), and U+200E/U+200F in the right-to-left faces.
     libass itself never looks default-ignorables up in a font (`ass_shaper_skip_characters`, which
     skips U+200B-200F and U+2060-206F), so the joiners cannot make the face choice depend on which
     script a track met first, and U+2060 (listed in plan 3.2) is not worth carrying at all.
  2. Layout kept: `layout_features=*`, `layout_scripts=*`, the closure follows from the cmap.
  3. Hinting dropped (libass renders unhinted; the archived Tibetan file carries TrueType
     instructions). `glyf`/`loca` stay in the form FreeType 2.4.11-2.7.0 can open (< 65535 glyphs and
     `loca` < 0x40000 bytes - asserted; these faces are opened by the bundled FreeType only, but
     `ass/fonts/tests.rs` checks every shipped face against the old bound anyway).
  4. **Win metrics normalised.** libass sizes a face from `usWinAscent + usWinDescent` and its own
     em, so a Noto face with tall win metrics would render smaller than Inter in the same line
     and change the line height. `usWinAscent/usWinDescent`, `hhea` and the typo metrics are
     rewritten to Inter Bold's ratios (2269/2048 and 660/2048 em, read from `pkg/appfont-bold.ttf`
     at run time), scaled to the face's upem.
  5. The name records above. Name IDs 0, 7, 13 and 14 (copyright, trademark, licence, licence URL)
     are asserted present: OFL 1.1 §2 wants the notice to travel with the Font Software, and none of
     these copyright statements declares a Reserved Font Name (asserted), so OFL §3 imposes no
     rename and "Noto Sans <script>" is kept.

**The inputs are PINNED**, by URL (a commit, never `main`), byte count and sha256, in `SOURCES`
below, and checked before anything is read. `--fetch` downloads them into a directory that must be
empty or absent; every other run reads that directory with `python3 -I`-style hygiene (the tool
never imports or executes anything from it). The output is deterministic for one fontTools version
(`recalcTimestamp=False`); the manifest records the version and each output's sha256, but the
semantics (coverage, ratio, names) are what `ass/fonts/tests.rs` asserts, not the byte hash.
"""
import hashlib
import json
import sys
import unicodedata
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import plxsubs_names as names  # noqa: E402

from fontTools import subset  # noqa: E402
from fontTools.ttLib import TTFont  # noqa: E402
from fontTools import version as FONTTOOLS_VERSION  # noqa: E402

REPO = Path(__file__).resolve().parent.parent
OUT_DIR = REPO / "pkg" / "fonts" / "subs"
MANIFEST = OUT_DIR / "manifest.json"
INTER_BOLD = REPO / "pkg" / "appfont-bold.ttf"

# --- the pins. Change URL, bytes and sha256 together or not at all. -----------------------------
MONOREPO = "https://raw.githubusercontent.com/notofonts/notofonts.github.io"
MONOREPO_COMMIT = "5c3913063bb189df4edc94b766584dfb4c04a187"  # 2026-10-14
ARCHIVE = "https://raw.githubusercontent.com/notofonts/noto-fonts"
ARCHIVE_COMMIT = "ffebf8c1ee449e544955a7e813c54f9b73848eac"


def monorepo_url(script: str) -> str:
    return f"{MONOREPO}/{MONOREPO_COMMIT}/fonts/NotoSans{script}/unhinted/ttf/NotoSans{script}-Bold.ttf"


def archive_url(script: str) -> str:
    return f"{ARCHIVE}/{ARCHIVE_COMMIT}/archive/hinted/NotoSans{script}/NotoSans{script}-Bold.ttf"


# script -> (bytes, sha256) of the upstream file named NotoSans<script>-Bold.ttf
SOURCES = {
    "Thai": (20600, "5227602d7f9108252cfb7d75d6f7b2a26bdb2fc2fa89a702f9063758fb2f86c7"),
    "Lao": (21508, "6fea2e9f231a4b9cfdeec90e9afaff8818b3d11f74cdb23559275f71eeff1ffd"),
    "Khmer": (67976, "817382a538f299eb809ee9313ef5c52e0049e3487eb4d02678349755ed607ddf"),
    "Myanmar": (146412, "c40a000f8093de13bdf1379a7c589faa2ea989319f33f244eaaa907033c6889f"),
    "Devanagari": (183412, "ff2f76a23aad41e0608c2d7dbc4bacd247ff3bec78f0ec2a8fb106b561636e58"),
    "Bengali": (102440, "2fe8d779e0f19576d1fc11e39f9154b4326a17b693215017dd1f5a8898c3f2c3"),
    "Gurmukhi": (35712, "d16804da302ecb2a7defe0502e65136870445fd88f9fddaef60b68d5f1ee7386"),
    "Gujarati": (140812, "3ce0cf6e1d0bfe2ef5823772eeb2f25da28ec0c2c1d99c0fce529025723e61a1"),
    "Tamil": (46428, "04a472e49fa83b387976756554fe179de631a950d79f652ebba14a39a44f5d71"),
    "Telugu": (165564, "44393b05a863057cc2cbfe86623fc20b906b00cf19a2ae9304cdd0f3608889db"),
    "Kannada": (133868, "e989c72f6fec5a0b3366af5f90e3ce8b2b8a350fc5286c6374f0353937e513f5"),
    "Malayalam": (75916, "165d635d82b0eb0a04275b92b9334c1ece679b99f263727c24c7c92ebcab51e9"),
    "Sinhala": (88508, "438ce1325e788ec0501042dd6e798c0ec02231a42dbf42e72d17b338aa46742a"),
    "Oriya": (130104, "1c575e8aba40d6baf200e8abac534e9517f431d38cc8131715643037eec149da"),
    "Tibetan": (393672, "5683e96d9d2a7986b8165e9e5b20367257b7f4fc6948d37751ca9f83456de27a"),
    "Arabic": (141852, "f4cb79f842a07d1bcf597879574358fb7010f73761396d1d0f2445dfedb4d2ca"),
    "Hebrew": (16900, "dfdb3056de1f4542b888c77a1a8a750548a802e271479f56e52152423b64dde8"),
    "Thaana": (27000, "476597290f449013cb07477b1089219db333c6af9ec0e436a74139ea9b2c8536"),
    "Georgian": (35820, "517722454bbe6587ada79a0509db8c9b292b6a40b025eebe69c151a6508efc7e"),
    "Armenian": (18752, "1dd206046cd3f41efddec93359a8d6590e5e2d5fcb036b9025725b560cefe6c6"),
    "Ethiopic": (288828, "6b06274ea730962522b1fcbfdcb16cb5a385aa36734f31f5c9d0ca910541e880"),
    "CanadianAboriginal": (93656, "e786d24b678fcb2bb68236c66e4fff31b676a7e5863a7073eac58edfd7f00f50"),
}
ARCHIVED = {"Tibetan"}

# --- the faces, IN ADD ORDER (this order is the manifest order and the renderer's add order). ----
# script, primary block (lo, hi, minimum codepoints the cut must keep in it), RTL, languages from
# languages.rs whose NATIVE-SCRIPT name or script is this one.
FACES = (
    ("Thai", (0x0E00, 0x0E7F, 80), False, ("th",)),
    ("Lao", (0x0E80, 0x0EFF, 70), False, ("lo",)),
    ("Khmer", (0x1780, 0x17FF, 100), False, ("km",)),
    ("Myanmar", (0x1000, 0x109F, 150), False, ("my",)),
    ("Devanagari", (0x0900, 0x097F, 100), False, ("hi", "mr", "ne", "sa", "pi", "bh", "ks", "sd")),
    ("Bengali", (0x0980, 0x09FF, 80), False, ("bn", "as")),
    ("Gurmukhi", (0x0A00, 0x0A7F, 70), False, ("pa",)),
    ("Gujarati", (0x0A80, 0x0AFF, 80), False, ("gu",)),
    ("Tamil", (0x0B80, 0x0BFF, 60), False, ("ta",)),
    ("Telugu", (0x0C00, 0x0C7F, 80), False, ("te",)),
    ("Kannada", (0x0C80, 0x0CFF, 80), False, ("kn",)),
    ("Malayalam", (0x0D00, 0x0D7F, 90), False, ("ml",)),
    ("Sinhala", (0x0D80, 0x0DFF, 80), False, ("si",)),
    ("Oriya", (0x0B00, 0x0B7F, 75), False, ("or",)),
    ("Tibetan", (0x0F00, 0x0FFF, 180), False, ("bo", "dz")),
    ("Arabic", (0x0600, 0x06FF, 200), True, ("ar", "fa", "ur", "ps", "ug", "ha")),
    ("Hebrew", (0x0590, 0x05FF, 50), True, ("he", "yi")),
    ("Thaana", (0x0780, 0x07BF, 50), True, ("dv",)),
    ("Georgian", (0x10A0, 0x10FF, 80), False, ("ka",)),
    ("Armenian", (0x0530, 0x058F, 80), False, ("hy",)),
    ("Ethiopic", (0x1200, 0x137F, 300), False, ("am", "ti")),
    ("CanadianAboriginal", (0x1400, 0x167F, 600), False, ("iu", "cr", "oj")),
)

# A script a listed language is written in, for which no Bold face is shipped: it falls to
# `path_default` (the CJK file), which has no glyph for it, so it draws as tofu. Every language of
# languages.rs lands in exactly one bucket (`language_buckets`), and a test holds that.
UNCOVERED = (
    {"script": "Yi", "languages": ["ii"],
     "reason": "Noto Sans Yi ships a Regular weight only; there is no Bold to match Inter Bold's colour"},
    {"script": "Avestan", "languages": ["ae"],
     "reason": "Noto Sans Avestan ships a Regular weight only; there is no Bold to match Inter Bold's colour"},
    {"script": "Mongolian", "languages": ["mn"],
     "reason": "Cyrillic Mongolian is Inter's; the traditional vertical script has only a Regular "
               "(Noto Sans Mongolian) and is not shipped"},
)
# Languages whose native name is drawn by the CJK default face (the file at `path_default`).
CJK_LANGUAGES = ("ja", "zh", "zh-TW", "ko")
LANGUAGES_RS = REPO / "rust-modules" / "plex" / "src" / "plex" / "languages.rs"
CJK_FACE = REPO / "pkg" / "appfont-cjk.ttf"

# Latin, digits, punctuation, currency, combining diacriticals: Inter Bold's. See the docstring.
INTER_RANGES = ((0x0000, 0x024F), (0x0300, 0x036F), (0x2000, 0x206F), (0x20A0, 0x20CF))
# U+2060 (word joiner) is NOT kept although plan 3.2 listed it: libass skips U+2060-206F (and
# U+200B-200F) as default-ignorables before any face lookup (`ass_shaper_skip_characters`), so a
# face glyph for it would never be used. A rendering test holds that a cue with it draws no box.
JOINERS = (0x200C, 0x200D, 0x25CC)
RTL_MARKS = (0x200E, 0x200F)
# A base letter whose rendered height the renderer test compares with its design height (the
# win-metrics normalisation is what makes the two agree).
PROBES = {"Thai": "\u0e01", "Devanagari": "\u0915"}
SAMPLE_LETTERS = 6
LICENCE_NAME_IDS = {0: "copyright", 7: "trademark", 13: "licence", 14: "licence URL"}
LOCA_LIMIT = 0x40000


def sha256_of(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def source_file(directory: Path, script: str) -> Path:
    return directory / f"NotoSans{script}-Bold.ttf"


def source_url(script: str) -> str:
    return archive_url(script) if script in ARCHIVED else monorepo_url(script)


def verify_source(path: Path, script: str) -> None:
    size, digest = SOURCES[script]
    blob = path.read_bytes()
    if len(blob) != size:
        raise SystemExit(f"{path.name}: {len(blob)} bytes, expected {size}")
    got = hashlib.sha256(blob).hexdigest()
    if got != digest:
        raise SystemExit(
            f"{path.name}: sha256 {got}\n  expected {digest}\n  This is a PINNED input "
            f"({source_url(script)}). Move the URL, bytes and sha256 together.")


def fetch(directory: Path) -> None:
    """Download every pinned input into `directory`, which must not hold anything else."""
    if directory.exists() and any(directory.iterdir()):
        raise SystemExit(f"{directory} is not empty: fetch into a NEW directory (downloads are untrusted)")
    directory.mkdir(parents=True, exist_ok=True)
    for script, _block, _rtl, _langs in FACES:
        url = source_url(script)
        dest = source_file(directory, script)
        with urllib.request.urlopen(url, timeout=120) as response:  # noqa: S310 - pinned https URL
            blob = response.read()
        dest.write_bytes(blob)
        verify_source(dest, script)  # a wrong byte count or hash deletes nothing but stops here
        print(f"fetched {dest.name}  {len(blob):,} bytes  sha256 {SOURCES[script][1][:12]}...")


def cmap_of(font: TTFont) -> set:
    cps = set()
    for table in font["cmap"].tables:
        if table.isUnicode():
            cps |= set(table.cmap)
    return cps


def inter_bold_facts():
    """(codepoints, upem, win ascent, win descent) of the committed Inter Bold."""
    font = TTFont(INTER_BOLD, lazy=True)
    os2 = font["OS/2"]
    return cmap_of(font), font["head"].unitsPerEm, os2.usWinAscent, os2.usWinDescent


def in_inter_ranges(cp: int) -> bool:
    return any(lo <= cp <= hi for lo, hi in INTER_RANGES)


def keep_set(upstream: set, inter: set, rtl: bool) -> set:
    keep = {cp for cp in upstream if not in_inter_ranges(cp) and cp not in inter}
    for cp in JOINERS + (RTL_MARKS if rtl else ()):
        if cp in upstream:
            keep.add(cp)
    return keep


def sample_of(codepoints, block: tuple) -> str:
    """Up to SAMPLE_LETTERS base letters from the script's block, evenly spaced: a string every
    shaped face must be able to draw with no .notdef (`ass/fonts/tests.rs` renders it)."""
    lo, hi, _floor = block
    letters = [cp for cp in sorted(codepoints)
               if lo <= cp <= hi and unicodedata.category(chr(cp)) in ("Lo", "Ll", "Lu", "Lm")]
    if len(letters) < SAMPLE_LETTERS:
        raise SystemExit(f"U+{lo:04X}-{hi:04X}: only {len(letters)} base letters")
    step = (len(letters) - 1) / (SAMPLE_LETTERS - 1)
    return "".join(chr(letters[round(i * step)]) for i in range(SAMPLE_LETTERS))


def probe_of(font: TTFont, script: str):
    """The probe letter's design bounds, in font units (yMin, yMax), or None."""
    char = PROBES.get(script)
    if char is None:
        return None
    glyph = font.getBestCmap()[ord(char)]
    g = font["glyf"][glyph]
    g.recalcBounds(font["glyf"])
    return {"char": char, "y_min": g.yMin, "y_max": g.yMax, "upem": font["head"].unitsPerEm}


def ranges_of(codepoints) -> list:
    out = []
    for cp in sorted(codepoints):
        if out and cp == out[-1][1] + 1:
            out[-1][1] = cp
        else:
            out.append([cp, cp])
    return out


def normalise_metrics(font: TTFont, inter_upem: int, inter_asc: int, inter_desc: int) -> tuple:
    upem = font["head"].unitsPerEm
    asc = round(inter_asc * upem / inter_upem)
    desc = round(inter_desc * upem / inter_upem)
    os2 = font["OS/2"]
    os2.usWinAscent, os2.usWinDescent = asc, desc
    os2.sTypoAscender, os2.sTypoDescender, os2.sTypoLineGap = asc, -desc, 0
    os2.fsSelection |= 1 << 7  # USE_TYPO_METRICS: every reader agrees on the same numbers
    font["hhea"].ascent, font["hhea"].descent, font["hhea"].lineGap = asc, -desc, 0
    return asc, desc


def check_licence(font: TTFont, label: str) -> None:
    have = {r.nameID for r in font["name"].names}
    missing = [why for nid, why in LICENCE_NAME_IDS.items() if nid not in have]
    if missing:
        raise SystemExit(f"{label}: name table lost the OFL notice chain: {', '.join(missing)}")
    for nid in (0, 13):
        text = font["name"].getDebugName(nid) or ""
        if "reserved font name" in text.lower():
            raise SystemExit(f"{label}: declares a Reserved Font Name; OFL §3 forbids keeping the name")


def cut_face(script: str, block: tuple, rtl: bool, src: Path, inter: tuple) -> tuple:
    inter_cps, upem_i, asc_i, desc_i = inter
    font = TTFont(src, recalcTimestamp=False)
    check_licence(font, src.name)
    upstream = cmap_of(font)
    keep = keep_set(upstream, inter_cps, rtl)

    opts = subset.Options()
    opts.layout_features = ["*"]
    opts.layout_scripts = ["*"]
    opts.name_IDs = ["*"]
    opts.name_languages = ["*"]
    opts.name_legacy = True
    opts.notdef_outline = True
    opts.notdef_glyph = True
    opts.hinting = False
    opts.glyph_names = False
    opts.legacy_cmap = False
    opts.symbol_cmap = False
    opts.drop_tables = [t for t in opts.drop_tables if t not in ("DSIG",)] + ["DSIG"]
    sub = subset.Subsetter(options=opts)
    sub.populate(unicodes=sorted(keep))
    sub.subset(font)

    asc, desc = normalise_metrics(font, upem_i, asc_i, desc_i)
    names.add_noto_aliases(font)

    after = cmap_of(font)
    if after != keep:
        raise SystemExit(f"{script}: subsetting changed coverage: -{sorted(keep - after)[:8]} +{sorted(after - keep)[:8]}")
    lo, hi, floor = block
    n_block = sum(1 for cp in after if lo <= cp <= hi)
    if n_block < floor:
        raise SystemExit(f"{script}: {n_block} codepoints in U+{lo:04X}-{hi:04X}, expected at least {floor}")
    if "GSUB" not in font and script not in ("Thaana",):
        raise SystemExit(f"{script}: GSUB was dropped; this face is shaped by HarfBuzz")
    n_glyphs = len(font.getGlyphOrder())
    if n_glyphs > 65534:
        raise SystemExit(f"{script}: {n_glyphs} glyphs: FreeType <= 2.7.0 cannot open >= 65535")
    if font["head"].indexToLocFormat == 1 and (n_glyphs + 1) * 4 >= LOCA_LIMIT:
        raise SystemExit(f"{script}: long loca too big for FreeType <= 2.7.0")
    check_licence(font, f"{script} (output)")
    # The English family is whatever upstream calls it ("Noto Sans Canadian Aboriginal" has spaces
    # the file name lacks); it is recorded, and `ass/fonts/tests.rs` holds it.
    family = font["name"].getDebugName(1)
    if not family.startswith("Noto Sans"):
        raise SystemExit(f"{script}: family {family!r} is not a Noto Sans face")
    sample = sample_of(after, block)
    probe = probe_of(font, script)
    out = OUT_DIR / f"NotoSans{script}-Bold.ttf"
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    font.save(out)
    return out, after, n_glyphs, asc, desc, family, sample, probe


def language_buckets(faces: list) -> dict:
    """Every language in languages.rs lands in exactly one bucket: a face (`faces[].languages`),
    `uncovered`, the CJK default, or Inter (the native name is wholly in Inter Bold's cmap). Returns
    the two buckets the manifest does not already hold, {"inter": [codes], "cjk": [codes]}. A
    language that fits none is an error: the gap is a decision to be written down, never silence."""
    import re
    rows = re.findall(r'code: "([A-Za-z-]+)", name: "([^"]*)"', LANGUAGES_RS.read_text())
    if len(rows) < 100:
        raise SystemExit(f"{LANGUAGES_RS}: parsed only {len(rows)} languages")
    by_code = dict(rows)
    inter_cps = inter_bold_facts()[0]
    cjk_cps = cmap_of(TTFont(CJK_FACE, lazy=True))
    explicit = {code for face in faces for code in face["languages"]}
    explicit |= {code for entry in UNCOVERED for code in entry["languages"]}
    explicit |= set(CJK_LANGUAGES)
    for code in explicit:
        if code not in by_code:
            raise SystemExit(f"{code}: named in FACES/UNCOVERED/CJK_LANGUAGES but not in languages.rs")
    buckets = {"inter": [], "cjk": list(CJK_LANGUAGES)}
    for code, name in rows:
        wanted = {ord(c) for c in name if not c.isspace()}
        if code in CJK_LANGUAGES:
            if not wanted <= cjk_cps:
                raise SystemExit(f"{code}: name {name!r} is not wholly in the CJK face")
        elif code in explicit:
            continue
        elif wanted <= inter_cps:
            buckets["inter"].append(code)
        else:
            lacking = sorted(f"U+{cp:04X}" for cp in wanted - inter_cps)
            raise SystemExit(f"{code} ({name!r}): no bucket - Inter Bold lacks {lacking}; add a face "
                             f"to FACES or the script to UNCOVERED")
    return buckets


def alias_inter_bold(write: bool = True) -> bool:
    """Idempotently give the committed Inter Bold its "PlxSubs" record. True if it needs/had a
    change; with `write=False` nothing is saved (the `--check` mode must not touch the tree)."""
    font = TTFont(INTER_BOLD, lazy=True, recalcTimestamp=False)
    before = [(r.platformID, r.platEncID, r.langID, r.nameID, str(r)) for r in font["name"].names]
    names.add_plxsubs_alias(font)
    after = [(r.platformID, r.platEncID, r.langID, r.nameID, str(r)) for r in font["name"].names]
    if sorted(before) == sorted(after):
        return False
    if write:
        font.save(INTER_BOLD)
    return True


def cut(directory: Path) -> None:
    for script, _b, _r, _l in FACES:
        path = source_file(directory, script)
        if not path.is_file():
            raise SystemExit(f"missing {path}; run with --fetch first")
        verify_source(path, script)
    if alias_inter_bold():
        print(f"{INTER_BOLD.name}: added the PlxSubs alias")
    inter = inter_bold_facts()
    faces = []
    for script, block, rtl, langs in FACES:
        out, cps, n_glyphs, asc, desc, family, sample, probe = cut_face(script, block, rtl, source_file(directory, script), inter)
        size, digest = SOURCES[script]
        faces.append({
            "file": out.name,
            "family": family,
            "script": script,
            "languages": list(langs),
            "rtl": rtl,
            "ranges": ranges_of(cps),
            "codepoints": len(cps),
            "glyphs": n_glyphs,
            "bytes": out.stat().st_size,
            "sha256": sha256_of(out),
            "win": [asc, desc],
            "sample": sample,
            "probe": probe,
            "upstream": {"url": source_url(script), "bytes": size, "sha256": digest},
        })
        print(f"{out.name:34s} {out.stat().st_size:>8,} bytes  {len(cps):>5,} codepoints  {n_glyphs:>5,} glyphs")
    manifest = {
        "schema": 1,
        "tool": "tools/cut-noto-subs.py",
        "fonttools": FONTTOOLS_VERSION,
        "alias": {
            "family": names.ALIAS_FAMILY,
            "plxsubs_lang_id": names.LANG_PLXSUBS,
            "inter_lang_id": names.LANG_INTER,
            "inter_bold": INTER_BOLD.name,
        },
        "inter_ranges": [list(r) for r in INTER_RANGES],
        "faces": faces,
        "uncovered": list(UNCOVERED),
        "buckets": language_buckets(faces),
    }
    MANIFEST.write_text(json.dumps(manifest, indent=1, ensure_ascii=False) + "\n")
    total = sum(f["bytes"] for f in faces)
    print(f"{MANIFEST.relative_to(REPO)}: {len(faces)} faces, {total:,} bytes")


def check() -> None:
    manifest = json.loads(MANIFEST.read_text())
    for face in manifest["faces"]:
        path = OUT_DIR / face["file"]
        if sha256_of(path) != face["sha256"]:
            raise SystemExit(f"{path.name} does not match the manifest; re-run the cut")
    if alias_inter_bold(write=False):
        raise SystemExit("pkg/appfont-bold.ttf lacks the PlxSubs alias; run the cut to add it")
    if language_buckets(manifest["faces"]) != manifest["buckets"]:
        raise SystemExit("the manifest's language buckets are stale; re-run the cut")
    print(f"{len(manifest['faces'])} faces match the manifest")


def main(argv: list) -> int:
    args = list(argv)
    if "--check" in args:
        check()
        return 0
    do_fetch = "--fetch" in args
    do_cut = "--cut" in args or not do_fetch
    args = [a for a in args if a not in ("--fetch", "--cut")]
    if len(args) != 1:
        print(__doc__.strip().splitlines()[2:6], file=sys.stderr)
        return 2
    directory = Path(args[0])
    if do_fetch:
        fetch(directory)
    if do_cut:
        cut(directory)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
