#!/usr/bin/env python3
"""Packaging-metadata assertions. Stdlib only, no NDK, no TV — runs anywhere.

The registry reads metadata straight out of the .ipk (webosbrew's repogen/ipk_file.py reads
Package/Version/Installed-Size from the control file, then appinfo.json), so any disagreement
between the three places the version is written is a submission failure rather than a warning.
"""
import json
import re
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FAILURES: list[str] = []


def check(cond: bool, msg: str) -> None:
    if cond:
        print(f"  ok — {msg}")
    else:
        FAILURES.append(msg)
        print(f"  FAIL — {msg}")


def png_size(p: Path) -> tuple[int, int]:
    return struct.unpack(">II", p.read_bytes()[16:24])


def font_family(p: Path) -> str:
    """nameID 1 out of a TrueType `name` table, without fontTools."""
    b = p.read_bytes()
    (numtables,) = struct.unpack(">H", b[4:6])
    for i in range(numtables):
        off = 12 + 16 * i
        if b[off:off + 4] == b"name":
            toff, = struct.unpack(">I", b[off + 8:off + 12])
            count, stroff = struct.unpack(">HH", b[toff + 2:toff + 6])
            for r in range(count):
                ro = toff + 6 + 12 * r
                pid, _enc, _lang, nid, ln, no = struct.unpack(">HHHHHH", b[ro:ro + 12])
                if nid == 1:
                    raw = b[toff + stroff + no: toff + stroff + no + ln]
                    # platform 0 (Unicode) and 3 (Windows) are both UTF-16BE; 1 (Mac) is not.
                    enc = "latin-1" if pid == 1 else "utf-16-be"
                    return raw.decode(enc, "replace")
    return "?"


print("== version / id consistency ==")
appinfo = json.loads((ROOT / "pkg/appinfo.json").read_text())
control = dict(
    line.split(": ", 1)
    for line in (ROOT / "ipkroot/ctl/control").read_text().splitlines()
    if ": " in line
)
check(appinfo["id"] == control["Package"],
      f'appinfo id == control Package ({appinfo["id"]})')
check(appinfo["version"] == control["Version"],
      f'appinfo version == control Version ({appinfo["version"]})')
# The Makefile derives IPK_VERSION from appinfo.json, so the built filename is the third witness.
built = sorted((ROOT / "pkg").glob("com.beb.plxnative_*_arm.ipk"))
if built:
    check(len(built) == 1, f"exactly one built ipk in pkg/ (saw {[p.name for p in built]})")
    m = re.fullmatch(r"com\.beb\.plxnative_([0-9][0-9.]*)_arm\.ipk", built[0].name)
    check(m is not None and m.group(1) == appinfo["version"],
          f"built ipk filename carries the appinfo version ({built[0].name})")
else:
    print("  SKIP — no built ipk in pkg/ (run `make ipk` first)")
check(re.fullmatch(r"\d+\.\d+\.\d+", appinfo["version"]) is not None,
      "version is exactly three integers (LG requirement)")
check(appinfo["type"] == "native", 'appinfo type == "native"')
check(not appinfo["id"].startswith(("com.palm", "com.webos", "com.lge", "com.palmdts")),
      "app id avoids LG's reserved prefixes")

print("== icons ==")
check(png_size(ROOT / "pkg/icon.png") == (80, 80), "icon.png is 80x80")
check(png_size(ROOT / "pkg/largeIcon.png") == (130, 130), "largeIcon.png is 130x130")
# `iconColor` paints the launcher tile BEHIND the icon, so a disagreement draws the icon as a
# hard-edged rectangle floating in a differently-coloured tile. Shipped that way until 2026-08-02
# (gold tile, black icon) and invisible in every file — it only exists once the system composites.
# The corner pixel is the icon's own background; anything within a couple of levels is the same
# colour to the eye and to a PNG optimiser.
corner = None
try:
    from PIL import Image
    corner = Image.open(ROOT / "pkg/largeIcon.png").convert("RGB").getpixel((1, 1))
except ImportError:
    print("  SKIP — Pillow absent; cannot compare iconColor against the icon background")
if corner is not None:
    want = appinfo["iconColor"].lstrip("#")
    declared = tuple(int(want[i:i + 2], 16) for i in (0, 2, 4))
    check(max(abs(a - b) for a, b in zip(corner, declared)) <= 2,
          f"iconColor {appinfo['iconColor']} matches the icon's own background rgb{corner}")

check(png_size(ROOT / "pkg/splash.png") == (1920, 1080),
      "splash.png is exactly 1920x1080 (splashBackground accepts no other size)")
check(appinfo.get("splashBackground") == "splash.png",
      "appinfo declares splashBackground: splash.png")

print("== shipped fonts ==")
# Landed 2026-08-01: Inter (SIL OFL 1.1). This is now a REAL gate, not an XFAIL — its job is to
# stop Monotype Arial coming back through a stale local copy, which is exactly how it would
# return (the files are named appfont*.ttf, so nothing about the filename reveals the swap).
ALLOWED = {"Inter", "Arimo", "Roboto", "Noto Sans", "Source Sans 3"}
for f in ("pkg/appfont.ttf", "pkg/appfont-bold.ttf"):
    fam = font_family(ROOT / f)
    check(fam in ALLOWED, f"{f} family={fam!r} is redistributable (allowed: {sorted(ALLOWED)})")
check((ROOT / "pkg/OFL.txt").exists(),
      "pkg/OFL.txt present — the OFL requires the licence to travel with the font")

print("== ipk payload ==")
expected = {
    "plxnative", "appinfo.json", "icon.png", "largeIcon.png", "splash.png",
    "appfont.ttf", "appfont-bold.ttf", "OFL.txt",
}
data_tar = ROOT / "ipkroot/data.tar.gz"
if data_tar.exists():
    import tarfile
    with tarfile.open(data_tar) as t:
        members = [m for m in t.getmembers() if m.isfile()]
        names = {Path(m.name).name for m in members}
        paths = {m.name.lstrip("./") for m in members}
        owners = {(m.uname, m.gname) for m in members}
    check(expected <= names, f"payload carries all {len(expected)} app files")
    # The Makefile's own comment records the ipk once shipping WITHOUT the fonts, silently
    # rendering the whole theme::size ladder in DroidSans.
    check(owners <= {("root", "root"), ("", "")},
          f"payload is not owned by the developer's account (saw {sorted(owners)})")
    # webOS's *package* descriptor, distinct from the app's appinfo.json. Absent from every ipk
    # built before 2026-08-02 and undetectable from the dev loop, which scp's into an app dir the
    # TV already has registered. Without it `appinstalld` unpacks nothing.
    check(f'usr/palm/packages/{appinfo["id"]}/packageinfo.json' in paths,
          "payload carries usr/palm/packages/<id>/packageinfo.json")
else:
    print("  SKIP — ipkroot/data.tar.gz absent (run `make ipk` first)")

print("== ar container ==")
# `ar rcD` (GNU) terminates short member names with '/', which appinstalld rejects outright:
# "Failed to extract package", error_code -5, before a single file is unpacked. Nothing else in
# the pipeline notices — webosbrew-ipk-verify reads such an archive happily.
if built:
    blob = built[0].read_bytes()
    check(blob[:8] == b"!<arch>\n", "starts with the ar global header")
    members, off = [], 8
    while off + 60 <= len(blob):
        name = blob[off:off + 16].decode("latin-1").rstrip()
        size = int(blob[off + 48:off + 58].decode("latin-1").strip() or 0)
        members.append(name)
        off += 60 + size + (size % 2)
    check(members == ["debian-binary", "control.tar.gz", "data.tar.gz"],
          f"members are the three bare names in order (saw {members})")

print()
if FAILURES:
    for f in FAILURES:
        print(f"::error::{f}")
    sys.exit(1)
print("all packaging assertions passed")
