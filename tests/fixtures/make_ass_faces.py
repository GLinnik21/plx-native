#!/usr/bin/env python3
"""Regenerate tests/fixtures/ass-faces/face-NN.ttf: 14 one-glyph faces of one family.

libass 0.17.5 caps the faces one ASS_Font can hold at ASS_FONT_MAX_FACES (10); the bundled build
patches that cap away (ci/patches/libass-0001-grow-font-faces.patch). The native test
`native_fourteen_faces_of_one_family_all_render` gives one renderer these 14 faces under the
family "PlxFaces", each covering one private-use codepoint (U+E000 + n), and shows one cue of 14
glyphs with every glyph drawn. Each glyph is a solid square (a different size per face, so a
face standing in for another is visible); .notdef is empty, so a glyph that fell through to
.notdef draws nothing.

The faces are committed (a few hundred bytes each) so `make check-ass` needs no fontTools:

    python3 -m venv /tmp/fonttools && /tmp/fonttools/bin/pip install fonttools
    /tmp/fonttools/bin/python tests/fixtures/make_ass_faces.py          # rewrite
    /tmp/fonttools/bin/python tests/fixtures/make_ass_faces.py --check  # compare, write nothing

Output is deterministic (fixed head timestamps), so --check is exact. The script is the source;
the .ttf files carry no licence of their own beyond this repository's.
"""
import sys
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

FACES = 14
FAMILY = 'PlxFaces'
FIRST_CODEPOINT = 0xE000
UPEM = 1000
OUT = Path(__file__).resolve().parent / 'ass-faces'
# 2026-01-01T00:00:00Z as a font timestamp (seconds since 1904-01-01).
STAMP = 3_850_070_400


def square(side):
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((0, side))
    pen.lineTo((side, side))
    pen.lineTo((side, 0))
    pen.closePath()
    return pen.glyph()


def build(index):
    name = 'uniE%03X' % index
    builder = FontBuilder(UPEM, isTTF=True)
    builder.setupGlyphOrder(['.notdef', name])
    builder.setupCharacterMap({FIRST_CODEPOINT + index: name})
    side = 400 + 20 * index
    builder.setupGlyf({'.notdef': TTGlyphPen(None).glyph(), name: square(side)})
    builder.setupHorizontalMetrics({'.notdef': (UPEM, 0), name: (UPEM, 0)})
    builder.setupHorizontalHeader(ascent=800, descent=-200)
    builder.setupNameTable({
        'familyName': FAMILY,
        'styleName': 'Regular',
        'uniqueFontIdentifier': '%s-%02d' % (FAMILY, index),
        'fullName': '%s %02d' % (FAMILY, index),
        'psName': '%s-%02d' % (FAMILY, index),
        'version': 'Version 1.000',
    })
    builder.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200,
                     fsType=0)
    builder.setupPost()
    builder.setupHead(created=STAMP, modified=STAMP)
    return builder.font


def main():
    check = '--check' in sys.argv[1:]
    OUT.mkdir(exist_ok=True)
    stale = []
    for index in range(FACES):
        path = OUT / ('face-%02d.ttf' % index)
        font = build(index)
        font.save(str(path.with_suffix('.tmp')))
        data = path.with_suffix('.tmp').read_bytes()
        path.with_suffix('.tmp').unlink()
        if check:
            if not path.is_file() or path.read_bytes() != data:
                stale.append(path.name)
        else:
            path.write_bytes(data)
    if stale:
        sys.exit('stale fixture faces (rerun without --check): ' + ', '.join(stale))
    print('ok: %d faces' % FACES)


if __name__ == '__main__':
    main()
