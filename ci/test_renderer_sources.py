#!/usr/bin/env python3
"""The privately bundled ASS renderer's own C sources never enter the application ELF.

`ci/build-libass.py` compiles `src/ass*.c` into libass-plx with FreeType's, HarfBuzz's and
FriBidi's headers; the Makefile's application `SRCS` is every other `src/*.c`. A renderer source
missing from that `filter-out` is compiled into `pkg/plxnative` without those headers, which on the
ARM cross-build fails (`ft2build.h: No such file`) and, were the headers ever reachable, would link a
second copy of renderer code into the app. Reads only the two files; no compiler needed.
"""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def recipe_sources():
    """Every `src/*.c` the renderer recipe hands its compiler."""
    text = (ROOT / "ci/build-libass.py").read_text()
    return set(re.findall(r"ROOT / '(src/[\w/]+\.c)'", text))


def app_excluded():
    """The files the Makefile's application `SRCS` filters out of `$(wildcard src/*.c)`."""
    text = (ROOT / "Makefile").read_text()
    m = re.search(r"^SRCS = \$\(filter-out ([^,]+),\$\(wildcard src/\*\.c\)\)", text, re.M)
    if not m:
        raise AssertionError("the Makefile's SRCS line is not `$(filter-out ...,$(wildcard src/*.c))`")
    return set(m.group(1).split())


class RendererSourcesStayOutOfTheApp(unittest.TestCase):
    def test_the_recipe_is_read(self):
        self.assertIn("src/ass.c", recipe_sources(), "the pattern no longer finds the recipe's sources")

    def test_every_renderer_source_is_filtered_out_of_the_application(self):
        missing = sorted(recipe_sources() - app_excluded())
        self.assertEqual(missing, [], "renderer sources compiled into pkg/plxnative too: add them to "
                         "the Makefile's SRCS filter-out")


if __name__ == "__main__":
    unittest.main()
