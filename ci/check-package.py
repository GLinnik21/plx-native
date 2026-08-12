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
# Cargo.toml is the FOURTH witness, and the one with a user-visible consequence: the diagnostics
# read-out prints `plex::identity::VERSION`, which is `env!("CARGO_PKG_VERSION")`, and that panel is
# designed to be photographed into a bug report. A bump that missed Cargo.toml would ship a package
# labelled 0.2.1 whose own on-screen version says 0.2.0 — precisely the disagreement `identity`
# exists to make impossible, and nothing checked it until a release nearly went out that way.
cargo = (ROOT / "rust-modules/Cargo.toml").read_text()
m = re.search(r'^version = "([^"]+)"', cargo, re.M)
check(m is not None and m.group(1) == appinfo["version"],
      f'Cargo.toml version == appinfo version ({appinfo["version"]})')

# No build machine's directory layout may ship inside the package.
#
# This exists because it happened: v0.2.1 went out with the maintainer's working directory baked
# into all three bundled FFmpeg libraries — FFmpeg records its whole configure invocation in
# libavutil — and with it the reproducibility claim in the release notes, on the one number a user
# has to check an unsigned download. `ci/check-elf.sh` only ever scanned `pkg/plxnative`, so
# nothing looked at the libraries beside it.
#
# The pattern is ANCHORED on a non-path character so ordinary URL fragments do not trip it: the
# app talks to plex.tv's `/api/v2/home/users`, which is not a build path.
# A build-machine path anywhere in the payload.
#
# Two calibration bugs are baked into the shape below, both found by running this against a release
# KNOWN to be dirty rather than assuming it worked:
#
#   * matching per-BLOB and allowing anything containing "webos-ndk" passes the very file it was
#     written for. FFmpeg records its whole configure invocation as ONE string, so the unavoidable
#     `--cross-prefix=/…/webos-ndk/…` sits beside the offending `--prefix=/…/plex-native-poc/…`
#     and one allowed token vouches for the other. v0.2.1's libraries pass that test.
#   * tokenising to fix it, without keeping the leading boundary, makes plex.tv's own
#     `/api/v2/home/users` read as `/home/users` and fails every build.
#
# So: extract each path WITH its boundary character, drop the boundary, and allow per PATH.
HOSTPATH = re.compile(rb"(?:^|[^A-Za-z0-9/_.-])(/(?:Users|home)/[A-Za-z0-9_./+-]+)")
# The NDK's own location cannot be removed — `--cross-prefix` must be absolute (the wrapper gcc
# dies when invoked through PATH), so it rides in FFmpeg's recorded configure string. It is
# identical on every CI runner, which is the reason releases must be BUILT by CI.
ALLOWED_PATH = re.compile(rb"webos-ndk|^/home/runner/")

PAYLOAD = ROOT / "ipkroot/data/usr/palm/applications/com.beb.plxnative"
for member in sorted(PAYLOAD.rglob("*")) if PAYLOAD.is_dir() else []:
    if not member.is_file():
        continue
    dirty = sorted({m for m in HOSTPATH.findall(member.read_bytes()) if not ALLOWED_PATH.search(m)})
    check(not dirty,
          f"{member.name} carries no build-machine path"
          + (f" (saw {dirty[0].decode(errors='replace')})" if dirty else ""))

# ---- the RELEASE is coherent, not just the package -------------------------------------------
#
# These live here rather than in a skill or a checklist for one reason: a skill is advisory and a
# gate is not. Every defect found in v0.2.1 got out because the gates were skipped — publishing by
# hand skipped them wholesale — so the response to that cannot itself be something a person has to
# remember to run.

# CI publishes the release body from this file, so a missing one means a release with no notes.
note = ROOT / f"docs/release-notes/v{appinfo['version']}.md"
check(note.exists(), f"docs/release-notes/v{appinfo['version']}.md exists (CI publishes the body from it)")

if note.exists():
    body = note.read_text()
    # The three values that CANNOT be written by a person, because they do not exist until the
    # release run does: the artifact's hash, the commit and the run. `release.yml`'s publish job
    # fills these from the artifacts themselves, so the note carries sentinels — and a note that
    # dropped one would publish a body with no hash in it at all, which `verify-published.sh`
    # would only catch once the release was already public. This is the pre-publish half.
    sentinels = ("__IPK_SHA256__", "__COMMIT__", "__RUN_URL__", "__IPK_SIZE__", "__INSTALLED_SIZE__")
    missing = [s for s in sentinels if s not in body]
    check(not missing, "the note carries the sentinels CI substitutes"
                       + (f" (missing {', '.join(missing)})" if missing else ""))
    # …and it must not carry a hand-typed one beside them. A literal 64-hex string in the committed
    # file is either a stale hash from a previous release or a number somebody typed, and both are
    # the defect class this standard exists to end (`docs/release-notes/README.md` §1.9).
    typed = re.findall(r"\b[0-9a-f]{64}\b", body)
    ffmpeg_sha = "7f607a00dd0d28a729d5a4811205812eef01cf6ef6155025febb6f36a9062d52"
    stray = [h for h in typed if h != ffmpeg_sha]
    check(not stray, "no hand-typed package hash in the note (only the pinned FFmpeg tarball's)"
                     + (f" (found {stray[0][:12]}…)" if stray else ""))
    # Every firmware the note NAMES must be one this repo has evidence for. A past note asserted
    # support for a "webOS 26" that does not exist; this is that gate.
    evidence = (ROOT / "tools/fwcompat.py").read_text() + (ROOT / "docs/webos5-port.md").read_text()
    unknown = sorted({v for v in re.findall(r"webOS (\d+(?:\.\d+)*)", body) if v not in evidence})
    check(not unknown, "every webOS version the note names has evidence in the repo"
                       + (f" (no evidence for {', '.join(unknown)})" if unknown else ""))

# WHICH CONFIGURATION produced what is in pkg/, which the two gates below both need. The Makefile
# writes `features:$(RUST_FEATFLAGS)` into this stamp at PARSE time, and it is the same witness
# `release.yml` greps to prove RELEASE=1 took. Matched WHOLE, not by substring: the "shots" recipe
# in the Makefile's header is `--no-default-features --features devtriggers`, which a substring
# test would grade as a release build and then fail for carrying exactly the surface it asked for.
# Only the two configurations this project actually ships are graded; anything else says so.
# NB the stamp moves at make PARSE time, so any bare `make <target>` after a RELEASE=1 build flips
# it to dev while ipkroot still holds the release binary. Both workflows build, package and check
# in one shot so they never see that; a by-hand run on a stale tree can, and the disagreement it
# then reports is true — repackage before believing anything else about that tree.
_stamp = ROOT / "pkg/.build-config"
BUILD = {"features:": "dev", "features:--no-default-features": "release"}.get(
    _stamp.read_text().strip() if _stamp.exists() else "")

# THIRD-PARTY-NOTICES must name exactly the libraries that ship. RELEASE=1 drops swscale, and the
# notices claimed it for two releases — an LGPL document describing a file that is not in the box.
#
# The grade is against the DISTRIBUTED set, which is not what `pkg/` holds: `ci.yml` packages a DEV
# build deliberately (a PR artifact you can sideload with the /tmp trigger surface on) and a dev
# build stages one library more. Grading pkg/ verbatim against a document written for the release
# payload is what turned every push to main red from 2026-08-10 to 2026-08-12 — the notices were
# corrected and the gate added in the same commit, and only the release job ever built the
# configuration the pair describes. Subtracting the dev-only set keeps ONE rule for both
# configurations, with no build-flag sniffing: a new library still has to be documented, and a
# documented one that stopped shipping still fails. Whether a RELEASE build really dropped them is
# the separate, narrower check below.
DEV_ONLY_SONAMES = {"libswscale-plx.so.10"}   # the dev capture stream's scaler; RELEASE=1 drops it
shipped = {p.name for p in (ROOT / "pkg").glob("*.so.*")}
if shipped:
    named = set(re.findall(r"`(lib[a-z]+-plx\.so\.\d+)`", (ROOT / "THIRD-PARTY-NOTICES.md").read_text()))
    distributed = shipped - DEV_ONLY_SONAMES
    check(distributed == named,
          "THIRD-PARTY-NOTICES names exactly the distributed libraries"
          + (f" (shipped-not-named={sorted(distributed-named)} named-not-shipped={sorted(named-distributed)})"
             if distributed != named else ""))
    # ...and a RELEASE build must carry none of the dev-only ones at all, which is the half the
    # subtraction above cannot see.
    if BUILD == "release":
        extra = sorted(shipped & DEV_ONLY_SONAMES)
        check(not extra, "a RELEASE build ships none of the dev-only libraries"
                         + (f" (found {', '.join(extra)})" if extra else ""))

# A dev build carries the /tmp trigger surface. `RELEASE=1` must be on EVERY make invocation, and
# any make without it deletes the release artifacts at parse time — so this is worth asserting on
# the bytes rather than trusting the command line that produced them.
#
# The witness has to be a string only a `devtriggers` build emits, and almost none are: `dev.rs`
# builds every trigger path with `format!("/tmp/plxnative-{name}")`, so no full trigger path is a
# literal anywhere. The previous witness here was b"plxnative-autoplay" and it matched NOTHING —
# in EITHER configuration — so from the day it was written this printed "ok — the packaged binary
# is a RELEASE build" over CI's dev build on every run, while release.yml's stamp grep carried the
# property alone. `dev.rs`'s DIAG list is the one place the full names are literals, it is
# `#[cfg(feature = "devtriggers")]`, and `plxnative-noidle` is not one of the four logs `main.c`
# writes unconditionally. Measured on the two shipped artifacts — published v0.3.0 .ipk: 0
# occurrences; CI's dev .ipk for 8827d32c: 2.
#
# GRADED FROM BOTH SIDES, which is the repair for the defect class rather than for the one string:
# a witness that cannot fail is not a gate. The dev leg asserts the marker is still emitted, so the
# day DIAG is renamed CI fails on the next push instead of quietly going vacuous again.
DEV_WITNESS = b"plxnative-noidle"
binary = ROOT / "ipkroot/data/usr/palm/applications/com.beb.plxnative/plxnative"
if binary.exists() and BUILD:
    has_dev = DEV_WITNESS in binary.read_bytes()
    if BUILD == "release":
        check(not has_dev, "the packaged binary is a RELEASE build (no dev triggers compiled in)")
    else:
        check(has_dev, "the packaged binary is the DEV build the stamp records — which is also what"
                       f" proves `{DEV_WITNESS.decode()}` still witnesses the trigger surface")
elif binary.exists():
    print("  SKIP — pkg/.build-config is neither shipped configuration; not grading the binary")

# The checksum file has to verify where a USER stands: they download it beside the .ipk, so a
# `pkg/` prefix in the line makes `shasum -a 256 -c` fail for everyone. It did, through v0.2.1.
sha_file = ROOT / "pkg/ipk.sha256"
if sha_file.exists():
    check(not any(l.split("  ")[-1].startswith("pkg/") for l in sha_file.read_text().splitlines() if l.strip()),
          "ipk.sha256 carries the bare filename, so `shasum -c` works beside the .ipk")

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
# The crate version is a THIRD copy of the same number: plex/identity.rs sends it to both Plex
# services as X-Plex-Version via env!("CARGO_PKG_VERSION"), so a build whose Cargo.toml disagreed
# with appinfo.json would report a version no release ever had.
cargo_ver = re.search(r'^version\s*=\s*"([^"]+)"', (ROOT / "rust-modules/Cargo.toml").read_text(), re.M)
check(cargo_ver is not None and cargo_ver.group(1) == appinfo["version"],
      f'rust-modules/Cargo.toml version == appinfo version ({appinfo["version"]})')

# Control-file provenance. None of this is read by opkg, and that is the point: it is what a
# human — a webosbrew reviewer, or a user running `opkg info` — sees about who ships this and
# under what terms. The Maintainer assertion exists because the field held a personal Gmail that
# travelled inside every distributed .ipk, and nothing would have caught its return.
check("Homepage" in control, "control declares a Homepage")

# The three fields webosbrew's ipk-verify reads to decide whether a package was built by a webOS
# packager. Any one missing and every submission report carries ":warning: This package looks
# hand-rolled. Please build it with `ares-package`." — the check itself still PASSES, so nothing
# fails and the warning simply rides along on the PR forever. The heuristic is presence-only
# (dev-toolbox-cli, common/ipk/src/ipk.rs: `PACKAGER_FIELDS.iter().any(|f| control.get(f).is_none())`).
#
# Installed-Size is written by mkipk.py at build time, so only the two static ones are asserted
# here. Values taken from what ares-package 2.4.0 itself emits, except the packager string, which
# names OUR packager rather than copying theirs — claiming to be ares when we are not would be the
# dishonest way to silence a warning. See mkipk.py's header for why we do not use ares-package.
for field in ("webOS-Package-Format-Version", "webOS-Packager-Version"):
    check(field in control, f"control declares {field}")
check(control.get("License") == "MIT", f'control License == MIT (saw {control.get("License")!r})')
check("@users.noreply.github.com" in control["Maintainer"] or "@gmail.com" not in control["Maintainer"],
      f'control Maintainer is not a personal mailbox ({control["Maintainer"]})')

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

print("== compliance artifacts ==")
# LGPL-2.1 §6 requires the notice AND the licence text to travel with the BINARY, so these are
# payload rather than repo decoration — a copy on GitHub does not discharge it for someone who
# received only the .ipk. release.yml's legal-gate refuses to publish without the first two.
for f in ("LICENSE", "TRADEMARKS.md", "THIRD-PARTY-NOTICES.md"):
    check((ROOT / f).exists(), f"{f} present")
# LICENSE must stay VERBATIM MIT. GitHub's `licensee` matches it against known licence texts by
# similarity, and this file previously carried the trademark reservation appended below the grant —
# which pushed it under the threshold, so the repository reported its licence as "Other". That
# misrepresents the terms in the one place most people look. The reservation lives in TRADEMARKS.md
# now; this assertion is what stops it drifting back.
_lic = (ROOT / "LICENSE").read_text()
check(_lic.rstrip().endswith("SOFTWARE."),
      "LICENSE is verbatim MIT (no appended text — it would read as 'Other' on GitHub)")
check("TRADEMARK" not in _lic.upper(), "LICENSE carries no trademark reservation (see TRADEMARKS.md)")
NEEDED_LICENCES = {
    "LGPL-2.1.txt": "FFmpeg, GLib, glibc — dynamically linked, §6 notice duty",
    "MIT.txt": "Feather/Heroicons and the MIT-elected Rust crates",
    "Apache-2.0.txt": "Material Icons, moxcms, pxfm, compiler_builtins",
    "LLVM-exception.txt": "compiler_builtins",
    "Unicode-3.0.txt": "the Unicode tables inside Rust core",
    "Zlib.txt": "nanosvg — vendored and compiled into the binary",
}
for name, why in NEEDED_LICENCES.items():
    p = ROOT / "licenses" / name
    check(p.exists() and p.stat().st_size > 200, f"licenses/{name} — {why}")

print("== ipk payload ==")
expected = {
    "plxnative", "appinfo.json", "icon.png", "largeIcon.png", "splash.png",
    "appfont.ttf", "appfont-bold.ttf", "OFL.txt",
    "THIRD-PARTY-NOTICES.md", "LICENSE", "TRADEMARKS.md", *NEEDED_LICENCES,
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
