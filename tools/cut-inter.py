#!/usr/bin/env python3
"""Cut the shipped `pkg/appfont*.ttf` statics from the Inter variable font.

    python3 tools/cut-inter.py path/to/Inter[opsz,wght].ttf

Why this is a script and not a one-off: the two fixes below restore conditions the UI was tuned
under for months, and BOTH are invisible in the font's appearance — a future re-cut that forgets
them reintroduces two silent regressions. Re-running this is the only supported way to change the
shipped fonts.

  1. wght 400/700, **opsz 18**. opsz 18 is the single point on Inter's optical-size axis where the
     theme's whole size ladder renders design-true under LIGHT hinting: regular is bar-heavy only
     at 26 (the carve-out `tools/font-hint-audit.py` already accepts) and bold is clean at every
     rung. It is also typographically right — low opsz is Inter's Text end, and a 1080p panel read
     at 3 m has a small angular size regardless of the px number.

  2. **Tabular figures, frozen into cmap.** Inter defaults to PROPORTIONAL digits ('1' = 0.397 em,
     '4' = 0.640 em — a 1.61:1 spread) where Arial was flat 0.5562. `player_hud.rs::draw_clock`
     measures a template with every digit replaced by '0' and centres the real string on that box;
     its own doc comment says this keeps the box "stable while digits tick instead of wobbling with
     proportional digit widths". That identity is ONLY true for tabular figures — with Inter as-cut
     the elapsed clock breathes ~5px every time a '1' enters or leaves. SDL2_ttf 2.0.x performs no
     OpenType feature selection at runtime, so `tnum` cannot be turned on when drawing: it has to be
     frozen at cut time. We do that by pointing the digit cmap entries at the `.tf` alternates the
     font already carries. THE SAME APPLIES TO ANY future feature (stylistic sets, disambiguation)
     — if it is not frozen here, it does not exist on the device.

  3. **A legacy `kern` table.** Inter ships kerning in GPOS only. SDL2_ttf 2.0.x kerns through
     `FT_Get_Kerning` gated on `FT_HAS_KERNING`, and both read the legacy `kern` table and never
     GPOS — so Arial was being kerned on this device and Inter, as cut, is not. Visible at HUD
     title size: Arial's "To" ink merges into one run because the T overhangs the o; unkerned Inter
     leaves a 4 px hole. We flatten the Latin/Cyrillic GPOS pairs into a format-0 `kern` table.
     Format 0's subtable length is a uint16, so it holds at most (65535-14)//6 = 10920 pairs; we
     keep the largest-magnitude pairs between glyphs the font can actually address from cmap.
"""
import sys
from pathlib import Path

from fontTools.ttLib import TTFont, newTable
from fontTools.ttLib.tables import _k_e_r_n as kern_mod
from fontTools.varLib import instancer

OPSZ = 18
MAX_KERN_PAIRS = (65535 - 14) // 6  # format-0 subtable length is a uint16


def freeze_tabular_figures(font: TTFont) -> int:
    """Point the digit cmap entries at the .tf alternates. Returns how many were remapped."""
    order = set(font.getGlyphOrder())
    remap = {}
    for cp, name in ((0x30 + i, n) for i, n in enumerate(
            "zero one two three four five six seven eight nine".split())):
        tf = f"{name}.tf"
        if tf in order:
            remap[cp] = tf
    if not remap:
        raise SystemExit("no .tf figure alternates in this font — refusing to cut a wobbling clock")
    for table in font["cmap"].tables:
        for cp, tf in remap.items():
            if cp in table.cmap:
                table.cmap[cp] = tf
    return len(remap)


def gpos_kern_pairs(font: TTFont) -> dict:
    """Every (left, right) -> x-advance adjustment in GPOS PairPos, both subtable formats."""
    pairs = {}
    if "GPOS" not in font or not font["GPOS"].table.LookupList:
        return pairs

    def adv(v):
        return getattr(v, "XAdvance", 0) if v is not None else 0

    for lookup in font["GPOS"].table.LookupList.Lookup:
        # 9 = extension; resolve to the real subtable
        subtables = []
        for st in lookup.SubTable:
            if lookup.LookupType == 9 and hasattr(st, "ExtSubTable"):
                subtables.append((st.ExtensionLookupType, st.ExtSubTable))
            else:
                subtables.append((lookup.LookupType, st))
        for ltype, st in subtables:
            if ltype != 2:
                continue
            if st.Format == 1:
                for first, ps in zip(st.Coverage.glyphs, st.PairSet):
                    for pv in ps.PairValueRecord:
                        if (v := adv(pv.Value1)):
                            pairs[(first, pv.SecondGlyph)] = v
            elif st.Format == 2:
                c1 = st.ClassDef1.classDefs if st.ClassDef1 else {}
                c2 = st.ClassDef2.classDefs if st.ClassDef2 else {}
                cover = set(st.Coverage.glyphs)
                by1, by2 = {}, {}
                for g in cover:
                    by1.setdefault(c1.get(g, 0), []).append(g)
                for g, k in c2.items():
                    by2.setdefault(k, []).append(g)
                by2.setdefault(0, [])
                for i, rec1 in enumerate(st.Class1Record):
                    for j, rec2 in enumerate(rec1.Class2Record):
                        if not (v := adv(rec2.Value1)):
                            continue
                        for g1 in by1.get(i, []):
                            for g2 in by2.get(j, []):
                                pairs.setdefault((g1, g2), v)
    return pairs


def add_legacy_kern(font: TTFont) -> tuple:
    """Flatten GPOS kerning into a format-0 `kern` table. Returns (kept, total)."""
    pairs = gpos_kern_pairs(font)
    if not pairs:
        raise SystemExit("no GPOS PairPos kerning found — nothing to flatten")
    # Only pairs whose glyphs are reachable from cmap: unreachable glyphs cannot be typed, so
    # spending scarce format-0 slots on them would evict pairs that actually render.
    reachable = set()
    for t in font["cmap"].tables:
        reachable.update(t.cmap.values())
    usable = {k: v for k, v in pairs.items() if k[0] in reachable and k[1] in reachable}

    # Selection order is by SCRIPT TIER, then magnitude — NOT magnitude alone. Sorting purely by
    # |value| looks principled and is a trap: the largest adjustments in Inter belong to exotic
    # glyph combinations, and they filled every one of the 10920 slots, leaving 26 ASCII pairs and
    # evicting exactly the ones that matter — "To", "Va", "Wa", "Yo" all came out unkerned. Tier by
    # what this UI actually renders: Latin first, then the punctuation around it, then Cyrillic.
    def cps_for(names):
        m = {}
        for t in font["cmap"].tables:
            for cp, g in t.cmap.items():
                m.setdefault(g, cp)
        return m
    cp_of = cps_for(None)

    def tier(pair):
        a, b = (cp_of.get(pair[0], 0x10FFFF), cp_of.get(pair[1], 0x10FFFF))
        hi = max(a, b)
        if hi < 0x80:
            return 0                      # ASCII — the overwhelming majority of UI text
        if hi < 0x180 or 0x2000 <= hi < 0x2070:
            return 1                      # Latin-1 / Ext-A + general punctuation
        if hi < 0x500:
            return 2                      # Greek + Cyrillic
        return 3
    ranked = sorted(usable.items(), key=lambda kv: (tier(kv[0]), -abs(kv[1])))
    kept = dict(ranked[:MAX_KERN_PAIRS])

    st = kern_mod.KernTable_format_0()
    st.coverage, st.version, st.format = 1, 0, 0
    st.kernTable = kept
    tbl = newTable("kern")
    tbl.version, tbl.kernTables = 0, [st]
    font["kern"] = tbl
    return len(kept), len(usable)


def main() -> int:
    if len(sys.argv) != 2:
        return print(__doc__) or 2
    src = Path(sys.argv[1])
    repo = Path(__file__).resolve().parent.parent
    for wght, style, out in ((400, "Regular", "appfont.ttf"), (700, "Bold", "appfont-bold.ttf")):
        f = TTFont(src)
        instancer.instantiateVariableFont(f, {"wght": wght, "opsz": OPSZ},
                                          inplace=True, updateFontNames=False)
        n = f["name"]
        for nid, val in ((1, "Inter"), (2, style), (4, f"Inter {style}"),
                         (6, f"Inter-{style}"), (16, "Inter"), (17, style)):
            n.setName(val, nid, 3, 1, 0x409)
            n.setName(val, nid, 1, 0, 0)
        digits = freeze_tabular_figures(f)
        kept, total = add_legacy_kern(f)
        dst = repo / "pkg" / out
        f.save(dst)
        print(f"  {out:16s} wght {wght} opsz {OPSZ} | {digits} tabular digits frozen "
              f"| kern {kept}/{total} pairs | {dst.stat().st_size // 1024} KB")
    print("\nRe-run tools/font-hint-audit.py — these are new files and the ladder must be re-confirmed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
