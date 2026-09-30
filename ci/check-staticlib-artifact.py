#!/usr/bin/env python3
"""Prove the archive `make` is about to link is the one cargo just reported.

usage: check-staticlib-artifact.py <cargo --message-format=json output> <archive path>

make decides what to link by a timestamp on the archive path, and cargo does not always write it
(a fresh unit only re-links the old file; a failed or skipped uplift leaves whatever was there).
Since the crate declares `crate-type = ["rlib"]`, the `.a` exists only because the Makefile asks
`cargo rustc --crate-type staticlib` for it, so a silent miss would link an OLD archive with no
comment. cargo names every file it produced or vouches for in its `compiler-artifact` records;
this requires exactly one `.a` for the app crate there and that the path make links is that file.
Timestamps cannot do this: a fresh build legitimately leaves the archive older than the recipe.
"""
import filecmp
import json
import os
import sys

CRATE = "plxnative_modules"


def artifacts(lines):
    found = []
    for line in lines:
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if not isinstance(record, dict) or record.get("reason") != "compiler-artifact":
            continue
        if (record.get("target") or {}).get("name") != CRATE:
            continue
        found += [f for f in record.get("filenames") or [] if f.endswith(".a")]
    return found


def main(argv):
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    with open(argv[1], encoding="utf-8", errors="replace") as handle:
        built = artifacts(handle)
    lib = argv[2]
    if len(built) != 1:
        print(f"error: cargo reported {len(built)} {CRATE} archives (need exactly 1): {built}",
              file=sys.stderr)
        return 1
    if not os.path.isfile(built[0]):
        print(f"error: cargo reported {built[0]} but it does not exist", file=sys.stderr)
        return 1
    if not os.path.isfile(lib):
        print(f"error: cargo did not write {lib}; make would link nothing or a stale file",
              file=sys.stderr)
        return 1
    if not (os.path.samefile(built[0], lib) or filecmp.cmp(built[0], lib, shallow=False)):
        print(f"error: {lib} is not the archive cargo just built ({built[0]}); refusing to link "
              "a stale one", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
