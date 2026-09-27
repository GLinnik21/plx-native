#!/usr/bin/env python3
"""Subset pkg/appfont.ttf and pkg/appfont-bold.ttf into WOFF2 for plxnative.com, and check that
the result actually covers every character the staged site renders.

    python3 tools/subset-site-fonts.py subset --out-dir _site/fonts
    python3 tools/subset-site-fonts.py check  --site-dir _site

Both responsibilities live in one script so the character set is defined in exactly one place:
a font that passes `check` but was subset with a different range than `subset` used is not a
thing that can happen here. `.github/workflows/pages.yml` runs `subset` right after staging
every other _site asset, then `check` right before "Check for missing assets" (which only
verifies references resolve to *a* file, not that the file's font can render them). Run the same
two commands locally to reproduce a CI failure.

Two full TTFs — pkg/appfont.ttf and pkg/appfont-bold.ttf — are Inter statics cut by
tools/cut-inter.py for the APP; do not edit them here. This script only reads them and writes a
web-sized derivative into _site/fonts/, discarded and regenerated on every deploy.

Dependencies: fontTools, brotli (brotli backs fontTools' own WOFF2 compressor) — both pinned in
tools/requirements-docs.txt.
    pip install -r tools/requirements-docs.txt

Unicode coverage (BASE_RANGES below) was picked by scanning every rendered site surface for
non-ASCII codepoints actually in use (curly quotes, dashes, the header's left arrow, &middot;,
etc.), then rounding out to whole general-purpose blocks so an editorial tweak — one more curly
quote, one more en dash — doesn't need a font re-cut. `check` is the backstop for anything this
missed: it fails the build, with the exact codepoints, rather than shipping a tofu box.
"""

from __future__ import annotations

import argparse
import html
import re
import sys
import unicodedata
from pathlib import Path

try:
    from fontTools import subset as ft_subset
    from fontTools.ttLib import TTFont
except ImportError:
    sys.exit(
        "subset-site-fonts.py needs fontTools (and brotli for WOFF2).\n"
        "    pip install -r tools/requirements-docs.txt"
    )

REPO_ROOT = Path(__file__).resolve().parent.parent

# (weight, source TTF, output WOFF2 basename) — the two faces @font-face in site/styles.css
# declares for the "PlxAppFont" family.
FACES = [
    (400, REPO_ROOT / "pkg" / "appfont.ttf", "appfont.woff2"),
    (700, REPO_ROOT / "pkg" / "appfont-bold.ttf", "appfont-bold.woff2"),
]

# Inclusive codepoint ranges kept in the subset. Single-codepoint entries are (cp, cp).
BASE_RANGES = [
    (0x0020, 0x007E),  # Basic Latin (space through ~) — the site's ASCII prose
    (0x00A0, 0x00FF),  # Latin-1 Supplement — nbsp, ©, §, ·, ×, accented Latin
    (0x0100, 0x017F),  # Latin Extended-A — headroom for future accented names/text
    (0x2000, 0x206F),  # General Punctuation — en/em dash, curly quotes, ellipsis, nbsp variants
    (0x2190, 0x21FF),  # Arrows — the header's back-arrow (&larr;) and friends
    (0x20AC, 0x20AC),  # Euro sign
    (0x2122, 0x2122),  # Trade mark sign
    (0x2212, 0x2212),  # Minus sign
]

# Characters found by `check` that BASE_RANGES didn't cover are unioned in here at subset time
# too, so `subset` and `check` can never disagree about what the font is supposed to hold. Kept
# empty on trunk: everything currently rendered already falls inside BASE_RANGES.
EXTRA_CODEPOINTS: set[int] = set()

# Layout features to keep: kern (GPOS pair kerning), liga/calt (standard ligatures and
# contextual substitution — e.g. correct accent composition), ccmp (glyph composition/
# decomposition, needed for some Latin Extended-A precomposed forms). This is the set the site
# actually needs for "kerning/ligatures" rather than the full '*': measured on pkg/appfont.ttf,
# '*' pulls in Inter's small-caps/oldstyle-figures/fraction/ordinal alternate glyphs and nearly
# doubles the subset (34.8 KB vs 20.7 KB for the regular weight) for features the site's plain
# prose never triggers. Tabular figures are already frozen straight into cmap by
# tools/cut-inter.py, so no digit-related feature needs to be kept here.
LAYOUT_FEATURES = "kern,liga,calt,ccmp"

CHECK_SOURCES_HTML_GLOB = "**/*.html"
CHECK_SOURCES_JS = "site.js"
CHECK_SOURCES_CSS = "styles.css"

# Characters `check` never expects to find in a font: whitespace/control codepoints, and the
# Unicode replacement/format characters HTML/JS produce as separators rather than glyphs.
_IGNORED_CATEGORIES = {"Cc", "Cf", "Zl", "Zp"}


def _unicodes_arg(extra: set[int]) -> str:
    ranges = list(BASE_RANGES)
    for cp in sorted(extra):
        ranges.append((cp, cp))
    return ",".join(
        f"U+{lo:04X}" if lo == hi else f"U+{lo:04X}-{hi:04X}" for lo, hi in ranges
    )


def _cmap_codepoints(font_path: Path) -> set[int]:
    font = TTFont(str(font_path), lazy=True)
    try:
        cps: set[int] = set()
        for table in font["cmap"].tables:
            if table.isUnicode():
                cps.update(table.cmap.keys())
        return cps
    finally:
        font.close()


def do_subset(out_dir: Path, extra: set[int]) -> int:
    out_dir.mkdir(parents=True, exist_ok=True)
    unicodes_arg = _unicodes_arg(extra)
    total_before = total_after = 0
    for weight, src, out_name in FACES:
        if not src.exists():
            sys.exit(f"subset-site-fonts.py: missing source font {src}")
        out_path = out_dir / out_name
        args = [
            str(src),
            f"--unicodes={unicodes_arg}",
            "--flavor=woff2",
            "--no-hinting",
            f"--layout-features={LAYOUT_FEATURES}",
            "--drop-tables+=DSIG",
            f"--output-file={out_path}",
        ]
        rc = ft_subset.main(args)
        if rc:
            sys.exit(f"subset-site-fonts.py: pyftsubset failed on {src} (exit {rc})")
        before = src.stat().st_size
        after = out_path.stat().st_size
        total_before += before
        total_after += after
        print(
            f"  weight {weight}: {src.name} {before:,} B -> {out_path} {after:,} B "
            f"({after / before:.1%})"
        )
    print(f"  total: {total_before:,} B -> {total_after:,} B ({total_after / total_before:.1%})")
    return 0


# ---------------------------------------------------------------------------
# Coverage check
# ---------------------------------------------------------------------------


class _TextExtractor:
    """Minimal HTML text-node + attribute-value extractor, stdlib html.parser only."""

    # Attributes whose value is user-visible text, not a URL/id/class/etc.
    TEXT_ATTRS = {"alt", "aria-label", "title", "placeholder", "content", "value"}

    def __init__(self) -> None:
        from html.parser import HTMLParser

        extractor = self
        chunks: list[str] = []

        class _P(HTMLParser):
            def handle_starttag(self, tag, attrs):
                for name, value in attrs:
                    if name in extractor.TEXT_ATTRS and value:
                        chunks.append(value)

            def handle_data(self, data):
                chunks.append(data)

        self._parser_cls = _P
        self._chunks = chunks

    def extract(self, text: str) -> str:
        parser = self._parser_cls()
        parser.feed(text)
        parser.close()
        return "".join(self._chunks)


def _text_from_html(path: Path) -> str:
    extractor = _TextExtractor()
    return extractor.extract(path.read_text(encoding="utf-8"))


_JS_STRING_RE = re.compile(
    r"""'((?:\\.|[^'\\])*)'|"((?:\\.|[^"\\])*)"|`((?:\\.|[^`\\])*)`""", re.DOTALL
)
_JS_LINE_COMMENT_RE = re.compile(r"//[^\n]*")
_JS_BLOCK_COMMENT_RE = re.compile(r"/\*.*?\*/", re.DOTALL)


def _text_from_js(path: Path) -> str:
    src = path.read_text(encoding="utf-8")
    # Strip comments first so a character used only in a comment (never rendered) doesn't force
    # a wider subset than the page actually needs.
    src = _JS_BLOCK_COMMENT_RE.sub(" ", src)
    src = _JS_LINE_COMMENT_RE.sub(" ", src)
    return "".join(m.group(1) or m.group(2) or m.group(3) or "" for m in _JS_STRING_RE.finditer(src))


_CSS_CONTENT_RE = re.compile(r"""content\s*:\s*(?:"((?:\\.|[^"\\])*)"|'((?:\\.|[^'\\])*)')""")


def _text_from_css(path: Path) -> str:
    src = path.read_text(encoding="utf-8")
    text_parts = []
    for m in _CSS_CONTENT_RE.finditer(src):
        raw = m.group(1) or m.group(2) or ""
        # CSS content: "\201C" is a hex escape for a codepoint, not the four literal characters.
        raw = re.sub(
            r"\\([0-9a-fA-F]{1,6})\s?", lambda mm: chr(int(mm.group(1), 16)), raw
        )
        text_parts.append(raw)
    return "".join(text_parts)


def collect_site_text(site_dir: Path) -> str:
    parts = []
    for html_path in sorted(site_dir.glob(CHECK_SOURCES_HTML_GLOB)):
        parts.append(html.unescape(_text_from_html(html_path)))
    js_path = site_dir / CHECK_SOURCES_JS
    if js_path.exists():
        parts.append(_text_from_js(js_path))
    css_path = site_dir / CHECK_SOURCES_CSS
    if css_path.exists():
        parts.append(_text_from_css(css_path))
    return "".join(parts)


def missing_codepoints(text: str, covered: set[int]) -> list[int]:
    missing = set()
    for ch in text:
        cp = ord(ch)
        if cp in covered:
            continue
        if unicodedata.category(ch) in _IGNORED_CATEGORIES:
            continue
        missing.add(cp)
    return sorted(missing)


def do_check(site_dir: Path, fonts_dir: Path, extra: set[int]) -> int:
    covered: set[int] | None = None
    for _weight, _src, out_name in FACES:
        font_path = fonts_dir / out_name
        if not font_path.exists():
            sys.exit(f"subset-site-fonts.py check: {font_path} does not exist — run `subset` first")
        cps = _cmap_codepoints(font_path)
        covered = cps if covered is None else covered & cps

    text = collect_site_text(site_dir)
    missing = missing_codepoints(text, covered or set())
    if missing:
        print("PlxAppFont is missing glyphs the staged site actually renders:", file=sys.stderr)
        for cp in missing:
            name = unicodedata.name(chr(cp), "<unnamed>")
            print(f"  U+{cp:04X} {chr(cp)!r} ({name})", file=sys.stderr)
        print(
            "Add the missing codepoint(s) to BASE_RANGES (or EXTRA_CODEPOINTS) in "
            "tools/subset-site-fonts.py and re-run `subset`.",
            file=sys.stderr,
        )
        return 1
    print(f"PlxAppFont covers every character in {site_dir} (checked {len(text):,} characters).")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    p_subset = sub.add_parser("subset", help="cut pkg/appfont*.ttf into web WOFF2s")
    p_subset.add_argument("--out-dir", type=Path, default=REPO_ROOT / "_site" / "fonts")

    p_check = sub.add_parser("check", help="fail if the staged site needs a glyph the subset lacks")
    p_check.add_argument("--site-dir", type=Path, default=REPO_ROOT / "_site")
    p_check.add_argument("--fonts-dir", type=Path, default=None)

    args = parser.parse_args(argv)

    if args.command == "subset":
        print("Subsetting PlxAppFont for the site:")
        return do_subset(args.out_dir, EXTRA_CODEPOINTS)
    if args.command == "check":
        fonts_dir = args.fonts_dir or (args.site_dir / "fonts")
        return do_check(args.site_dir, fonts_dir, EXTRA_CODEPOINTS)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
