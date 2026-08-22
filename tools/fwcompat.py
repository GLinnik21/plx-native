#!/usr/bin/env python3
"""Resolve this binary against 14 real LG firmwares, offline, on the dev machine.

WHY THIS EXISTS. The error class that matters for portability is invisible at link time: the app
resolves FFmpeg, libcurl and libAcbAPI at RUNTIME and bundles its own FFmpeg, so nothing in the
build can tell you a television lacks a library or a symbol — the dynamic loader finds out, at
`exec()`, with nothing in the event log.
The only previous check for that was `webosbrew-ipk-verify`, which ships as an arm64 `.deb` and so
runs in CI on Linux and NOWHERE on this Mac. That put a full push-and-wait between "I changed a
`#[link]`" and "I know whether it still loads".

This is that check, locally. It reads the same firmware databases the webosbrew tool does — the
`webosbrew-toolbox-fw-symbols` package, which is not a checker but an INVENTORY: for 14 real
firmware images, every library, its `DT_NEEDED`, and its complete exported-symbol list, keyed by
webOS release. Nothing here is inferred; a "yes" means the symbol is in that firmware's table.

WHAT IT CAN AND CANNOT TELL YOU. It answers exactly one question — *would the dynamic loader be
able to start this binary, and would every symbol it imports resolve?* That question is the whole
of the webOS 5 port's first blocker and it is worth automating. It says NOTHING about behaviour:
a firmware can export `AcbAPI_setMediaVideoData` and still refuse to put a picture on the video
plane. Do not read a green matrix as "it works on webOS 5"; read it as "it starts".

USAGE
    tools/fwcompat.py                          # grade pkg/plxnative against every release
    tools/fwcompat.py --release 5.3.1          # one release, with the full missing list
    tools/fwcompat.py --lib libSDL2-2.0.so.0 --grep webOS      # what does that library export?
    tools/fwcompat.py --inventory libAcbAPI libavformat        # which releases carry these?

The database (~317 MB unpacked) is fetched once into ~/.cache/plxnative/fwsym and reused. Pass
--db to point at an existing extraction. Everything after the download is offline.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import urllib.request
from pathlib import Path

# Pinned so a database refresh is a visible commit rather than a silent change of verdict.
FWSYM_TAG = "v20260731-e1bb0c0"
FWSYM_DEB = "webosbrew-toolbox-fw-symbols_0.4.0-1_arm64.deb"
FWSYM_URL = f"https://github.com/webosbrew/dev-toolbox-cli/releases/download/{FWSYM_TAG}/{FWSYM_DEB}"

REPO = Path(__file__).resolve().parent.parent
DEFAULT_BINARY = REPO / "pkg" / "plxnative"
CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "plxnative" / "fwsym"

# The NDK's readelf understands the target's ELF; the host's may not exist at all on macOS.
NDK = Path(os.environ.get("WEBOS_SDK", Path.home() / "webos-ndk" / "arm-webos-linux-gnueabi_sdk-buildroot"))


def die(msg):
    print(f"fwcompat: {msg}", file=sys.stderr)
    sys.exit(2)


# ---------------------------------------------------------------- database


def ensure_db(db_arg):
    """Return the directory holding one subdirectory per firmware, downloading if needed."""
    if db_arg:
        d = Path(db_arg)
        if not d.is_dir():
            die(f"--db {d} is not a directory")
        return d
    data = CACHE / "data"
    if data.is_dir() and any(data.iterdir()):
        return data
    CACHE.mkdir(parents=True, exist_ok=True)
    deb = CACHE / FWSYM_DEB
    if not deb.exists():
        print(f"fetching firmware symbol databases (~25 MB) -> {deb}", file=sys.stderr)
        try:
            urllib.request.urlretrieve(FWSYM_URL, deb)
        except Exception as e:  # noqa: BLE001 - any failure here is the same user-facing problem
            die(f"could not download {FWSYM_URL}: {e}\n"
                f"        Fetch it by hand and re-run, or pass --db <extracted data dir>.")
    print("extracting...", file=sys.stderr)
    work = CACHE / "unpack"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    # A .deb is an `ar` archive; macOS ships `ar`, and its data member is tar.xz.
    subprocess.run(["ar", "x", str(deb), "data.tar.xz"], cwd=work, check=True)
    with tarfile.open(work / "data.tar.xz") as tf:
        tf.extractall(work)  # noqa: S202 - first-party package from a pinned release URL
    src = work / "usr" / "share" / "webosbrew" / "compat-checker" / "data"
    if not src.is_dir():
        die(f"unexpected package layout: {src} missing")
    shutil.rmtree(data, ignore_errors=True)
    shutil.move(str(src), str(data))
    shutil.rmtree(work, ignore_errors=True)
    deb.unlink(missing_ok=True)
    return data


def relver(s):
    """Sort key for a webOS release string. 4.10.0 is ABOVE 4.9 — this is why it is not a float."""
    return tuple(int(x) for x in s.split("."))


class Firmware:
    def __init__(self, path):
        self.path = path
        info = json.loads((path / "info.json").read_text())
        self.release = info["release"]
        self.version = info["version"]
        self.ota_id = info["ota_id"]
        self._index = None
        self._libs = {}

    @property
    def index(self):
        """alias (SONAME, real name, bare name) -> per-library json filename."""
        if self._index is None:
            self._index = json.loads((self.path / "index.json").read_text())
        return self._index

    def lib(self, alias):
        """The library record an alias resolves to, or None if this firmware lacks it."""
        fn = self.index.get(alias)
        if fn is None:
            return None
        if fn not in self._libs:
            self._libs[fn] = json.loads((self.path / fn).read_text())
        return self._libs[fn]

    def closure(self, roots):
        """Transitively resolve DT_NEEDED from `roots`. Returns (records, missing sonames)."""
        seen, out, missing = set(), [], []
        queue = list(roots)
        while queue:
            name = queue.pop()
            if name in seen:
                continue
            seen.add(name)
            rec = self.lib(name)
            if rec is None:
                missing.append(name)
                continue
            out.append(rec)
            queue.extend(rec.get("needed", ()))
        return out, missing


def load_firmwares(db):
    fws = [Firmware(p) for p in sorted(db.iterdir()) if (p / "info.json").exists()]
    if not fws:
        die(f"no firmware databases under {db}")
    return sorted(fws, key=lambda f: relver(f.release))


# ---------------------------------------------------------------- the ELF side


def readelf():
    cand = [
        NDK / "bin" / "arm-webos-linux-gnueabi-readelf",
        Path("/usr/bin/readelf"),
    ]
    for c in cand:
        if c.exists():
            return str(c)
    for name in ("llvm-readelf", "readelf", "eu-readelf"):
        p = shutil.which(name)
        if p:
            return p
    die("no readelf found. Install the webOS NDK (`make setup-env`) or put readelf on PATH.")


def elf_facts(binary):
    """(sorted DT_NEEDED, sorted undefined dynamic symbol names).

    Symbol versions are stripped: the databases store `name@VER` and a version tag that differs
    between firmwares is not by itself an incompatibility we can grade here.

    WEAK undefined symbols are EXCLUDED, and getting that wrong is how this tool first read as a
    total failure. An unresolved weak reference binds to 0 and the loader carries on — that is the
    entire point of the binding. Rust's std leans on it heavily: `statx`, `getrandom`,
    `copy_file_range`, `__clock_gettime64` and the rest are probed at runtime and fall back when
    the host glibc is too old, which is exactly the case here (built against 2.12 headers). Count
    them and every firmware, including the two the app demonstrably runs on, reports 14 missing
    symbols and the tool grades a working binary as broken.
    """
    re_ = readelf()
    needed = []
    dyn = subprocess.run([re_, "-d", "-W", str(binary)], capture_output=True, text=True, check=True).stdout
    for m in re.finditer(r"\(NEEDED\)\s+Shared library:\s+\[([^\]]+)\]", dyn):
        needed.append(m.group(1))

    undef = set()
    syms = subprocess.run([re_, "--dyn-syms", "-W", str(binary)], capture_output=True, text=True, check=True).stdout
    for line in syms.splitlines():
        f = line.split()
        # Num: Value Size Type Bind Vis Ndx Name
        if len(f) < 8 or not f[0].endswith(":"):
            continue
        if f[6] != "UND" or f[4] == "WEAK":
            continue
        name = f[7].split("@")[0]
        if name:
            undef.add(name)
    return sorted(needed), sorted(undef)


# ---------------------------------------------------------------- reporting


def grade(fw, needed, undef):
    """(missing libraries, missing symbols) for one firmware."""
    records, missing_libs = fw.closure(needed)
    exported = set()
    for rec in records:
        for s in rec.get("symbols", ()):
            exported.add(s.split("@")[0])
    missing_syms = sorted(s for s in undef if s not in exported)
    return missing_libs, missing_syms


def cmd_grade(args, db):
    binary = Path(args.binary)
    if not binary.exists():
        die(f"{binary} not found — run `make` first, or pass a path.")
    needed, undef = elf_facts(binary)
    print(f"{binary}: {len(needed)} DT_NEEDED, {len(undef)} undefined dynamic symbols\n")

    every = load_firmwares(db)
    fws = every
    if args.release:
        fws = [f for f in every if f.release in args.release]
        if not fws:
            die(f"no such release; have {', '.join(f.release for f in every)}")

    floor = relver(args.min_release) if args.min_release else None
    # Graded ONCE per firmware. The detail block below used to re-grade the selected release, so
    # the table and the detail could in principle disagree about the same binary.
    rows = [(fw, *grade(fw, needed, undef)) for fw in fws]
    print(f"{'release':<9} {'verdict':<8} {'missing libraries':<52} symbols")
    print("-" * 96)
    worst = 0
    for fw, mlibs, msyms in rows:
        ok = not mlibs and not msyms
        # Releases below the floor are graded and PRINTED but do not set the exit status. The five
        # oldest (webOS 1.2.0 through 3.9.2) fail permanently and for reasons nobody intends to
        # fix — they predate the C++11 std::string ABI, so StarfishMediaAPIs::Feed has a different
        # mangling — so counting them would make this tool useless as a gate while hiding the
        # regression it exists to catch.
        counts = floor is None or relver(fw.release) >= floor
        if counts and not ok:
            worst = 1
        libs = ", ".join(mlibs) if mlibs else "-"
        if len(libs) > 51:
            libs = libs[:48] + "..."
        mark = "" if counts else "  (below --min-release; not gated)"
        print(f"{fw.release:<9} {'OK' if ok else 'FAIL':<8} {libs:<52} {len(msyms) or '-'}{mark}")

    if args.release and len(rows) == 1:
        fw, mlibs, msyms = rows[0]
        if mlibs:
            print(f"\nmissing libraries on {fw.release}:")
            for x in mlibs:
                print(f"  {x}")
        if msyms:
            print(f"\nmissing symbols on {fw.release} ({len(msyms)}):")
            for x in msyms:
                print(f"  {x}")
    return worst


def cmd_lib(args, db):
    pat = re.compile(args.grep, re.I) if args.grep else None
    for fw in load_firmwares(db):
        rec = fw.lib(args.lib)
        if rec is None:
            print(f"{fw.release:<9} ABSENT")
            continue
        syms = [s for s in rec["symbols"] if not pat or pat.search(s)]
        print(f"{fw.release:<9} {rec['name']:<28} {len(rec['symbols'])} symbols"
              + (f", {len(syms)} matching" if pat else ""))
        for s in sorted(syms):
            print(f"            {s}")
    return 0


def cmd_inventory(args, db):
    fws = load_firmwares(db)
    print(f"{'release':<9} " + " ".join(f"{n[:22]:<24}" for n in args.inventory))
    print("-" * (9 + 25 * len(args.inventory)))
    for fw in fws:
        cells = []
        for name in args.inventory:
            hits = sorted(k for k in fw.index if k.startswith(name))
            # Prefer the fully-versioned real name — it carries the version we care about — but
            # among SIBLINGS of the library asked for, not among everything sharing its prefix.
            # `max(hits, key=len)` alone answers `libpf` with `libpf-miracastplugin.so.1.0.0`,
            # because that name is longer than `libpf-1.0.so.1`: a plugin, silently charted in
            # place of the media pipeline. Prefer the SHORTEST stem (the library itself, not a
            # `-something` sibling) and only then the longest version suffix on that stem.
            if hits:
                stem = min((k.split(".so")[0] for k in hits), key=len)
                real = max((k for k in hits if k.split(".so")[0] == stem), key=len)
            else:
                real = None
            cells.append(f"{real or '-':<24}")
        print(f"{fw.release:<9} " + " ".join(cells))
    return 0


def main():
    ap = argparse.ArgumentParser(
        description="Resolve an ELF against 14 real LG firmware library/symbol inventories, offline.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__.split("USAGE")[1] if "USAGE" in __doc__ else None,
    )
    ap.add_argument("binary", nargs="?", default=str(DEFAULT_BINARY))
    ap.add_argument("--db", help="an already-extracted compat-checker data directory")
    ap.add_argument("--release", action="append", help="grade only this release (repeatable)")
    ap.add_argument(
        "--min-release",
        metavar="REL",
        help="exit non-zero only for failures at or above this release (e.g. 4.4.2). "
        "Everything is still graded and printed; this decides what counts as a regression.",
    )
    ap.add_argument("--lib", help="dump one library's presence and symbols across releases")
    ap.add_argument("--grep", help="with --lib, filter symbols by this regex")
    ap.add_argument("--inventory", nargs="+", help="show which releases carry these library prefixes")
    args = ap.parse_args()

    db = ensure_db(args.db)
    if args.lib:
        return cmd_lib(args, db)
    if args.inventory:
        return cmd_inventory(args, db)
    return cmd_grade(args, db)


if __name__ == "__main__":
    sys.exit(main())
