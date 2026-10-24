#!/usr/bin/env python3
"""Fail when a crate the app library is built from is missing from the Makefile's RUST_INPUTS.

`$(RUST_LIB)` is an ordinary make timestamp rule: cargo is only invoked when one of its
prerequisites is newer than the library, and `RUST_INPUTS` (the `find` over a hand-kept list of
directories) is how a source edit becomes one. A crate left out of that list is a crate whose edits
`make` never sees: `make` / `make ipk` answer "up to date" and package the PREVIOUS library, with
nothing in the output to say so. Cargo's own fingerprinting cannot catch it, because cargo is never
asked. A downstream fork lost a day to exactly this after adding a crate to the workspace and not to
the list.

So this reads the app package's `path` dependencies out of `rust-modules/Cargo.toml`, follows each
crate's own `path` dependencies (normal and build, not dev: dev-dependencies are not linked into the
library), and demands that every crate directory reached is one RUST_INPUTS searches. It also fails
on a directory the list names that does not exist, which is a list gone stale the other way.

The storage helper is a separate binary with its own deliberately narrower STORAGE_INPUTS, and is
not the app package's dependency, so it is out of scope here.

    ci/check-rust-inputs.py               # the gate (make check-python runs it)
    ci/check-rust-inputs.py --root DIR    # check another checkout (the self-test's fixtures)

Exit 0 green, 1 on any failure.
"""
from pathlib import Path
import argparse
import re
import sys
import tomllib

DEP_TABLES = ("dependencies", "build-dependencies")


def listed_dirs(makefile: str) -> list[str]:
    """The directories RUST_INPUTS's `find` searches, as written (repo-relative)."""
    m = re.search(r"^RUST_INPUTS\s*:=\s*\$\(shell find (.*?) -type f", makefile, re.M)
    if not m:
        raise SystemExit("check-rust-inputs: no `RUST_INPUTS := $(shell find ... -type f` line in the Makefile")
    return [d.rstrip("/") for d in m.group(1).split()]


def path_deps(manifest: Path) -> list[Path]:
    """The crate directories `manifest` depends on by `path`, resolved against its own directory."""
    doc = tomllib.loads(manifest.read_text())
    tables = [doc.get(t, {}) for t in DEP_TABLES]
    for target in doc.get("target", {}).values():
        tables.extend(target.get(t, {}) for t in DEP_TABLES)
    out = []
    for table in tables:
        for spec in table.values():
            if isinstance(spec, dict) and "path" in spec:
                out.append((manifest.parent / spec["path"]).resolve())
    return out


def app_crates(root: Path) -> list[Path]:
    """Every crate directory the app package reaches through `path` dependencies, transitively."""
    seen: list[Path] = []
    queue = path_deps(root / "rust-modules" / "Cargo.toml")
    while queue:
        crate = queue.pop(0)
        if crate in seen:
            continue
        seen.append(crate)
        queue.extend(path_deps(crate / "Cargo.toml"))
    return seen


def failures(root: Path) -> list[str]:
    root = root.resolve()
    listed = listed_dirs((root / "Makefile").read_text())
    covered = [(root / d).resolve() for d in listed]
    out = []
    for d in listed:
        if not (root / d).is_dir():
            out.append(f"RUST_INPUTS names {d}/, which does not exist")
    # The app package's own sources: its manifest sits in rust-modules/, its code in src/.
    if (root / "rust-modules" / "src").resolve() not in covered:
        out.append("RUST_INPUTS does not search rust-modules/src, the app package's own sources")
    for crate in app_crates(root):
        if not any(crate == c or c in crate.parents for c in covered):
            rel = crate.relative_to(root) if crate.is_relative_to(root) else crate
            out.append(
                f"{rel}/ is a path dependency of the app library but RUST_INPUTS does not search it: "
                "an edit there would leave `make` linking the previous library"
            )
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = ap.parse_args()
    bad = failures(args.root)
    for line in bad:
        print(f"check-rust-inputs: {line}", file=sys.stderr)
    if bad:
        return 1
    print(f"check-rust-inputs: ok, RUST_INPUTS covers all {len(app_crates(args.root))} path crates of the app library")
    return 0


if __name__ == "__main__":
    sys.exit(main())
