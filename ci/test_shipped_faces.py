#!/usr/bin/env python3
"""Every face the text chain opens is shipped, staged and bundled wherever the others are.

`rust-modules/gfx/src/text.rs` names the faces it opens out of the app directory (`appfont*.ttf`).
Each list below copies files into some install or bundle by name, so a face added to the chain but
not to one of them is missing exactly there: on the television (the .ipk payload, and the package
gate that grades it), in a simulator bundle, in the GPL source bundle, or in the inputs that decide
when a documentation still is stale. A missing face draws no error; its script just becomes boxes.
"""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FACE = r"appfont[\w-]*\.ttf"


def chain_faces():
    return set(re.findall(r'"(' + FACE + r')"', (ROOT / "rust-modules/gfx/src/text.rs").read_text()))


def block(path, start, end):
    """The text of `path` from the first match of `start` up to the next match of `end`."""
    text = (ROOT / path).read_text()
    m = re.search(start, text, re.M)
    if not m:
        raise AssertionError(f"{path}: no {start!r}")
    e = re.compile(end, re.M).search(text, m.end())
    return text[m.start():e.start() if e else len(text)]


# (what it is, file, where its list starts, where it ends)
LISTS = [
    ("the .ipk payload (Makefile APP_FILES)", "Makefile", r"^APP_FILES = ", r"^\s*$"),
    ("the package gate's expected payload", "ci/check-package.py", r"^expected = \{", r"^\}"),
    ("the package gate's font licence check", "ci/check-package.py", r'^for f in \("pkg/appfont', r":$"),
    ("the macOS simulator bundle", "ci/mkmacapp.py", r'for f in \("appfont', r"\):$"),
    ("the Windows/WSL simulator staging", "tools/sim.ps1", r"for file in appfont", r"; do$"),
    ("the GPL source bundle's pkg/ files", "ci/source_bundle.py", r"^PKG_FILES = \{", r"\}"),
    ("the documentation stills' inputs", "tools/site_stills.py", r"^TREE_PATHS = \(", r"^\)"),
]


class ShippedFaces(unittest.TestCase):
    def test_the_chain_is_read(self):
        self.assertIn("appfont-cjk.ttf", chain_faces())

    def test_every_list_names_every_face_the_chain_opens(self):
        faces = chain_faces()
        for what, path, start, end in LISTS:
            with self.subTest(what):
                listed = set(re.findall(FACE, block(path, start, end)))
                self.assertEqual(sorted(faces - listed), [], f"{what} ({path}) lacks these faces")


if __name__ == "__main__":
    unittest.main()
