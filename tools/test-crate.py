#!/usr/bin/env python3
"""Build and run only the lib tests of the crate(s) you touched: `make test-crate C=plx_ui`.

    tools/test-crate.py --suite 'plxnative-modules plx_base ...' [--cargo 'cargo +nightly']
                        [--deps] [--filter NAME] [--dry-run] CRATE [CRATE ...]

An edit in a low crate makes `cargo test --lib -p <all 14>` rebuild and relink every dependent
test binary, ~20 s, when the developer only wants that crate's tests. This selects `-p CRATE`
alone (plus, with `--deps`, the workspace crates that DIRECTLY depend on it, read from
`cargo metadata` -- never a list kept here) and hands that to tools/cargo-test-parallel.py.

**The feature set is the part that has to be exactly right.** The application crate's default
features (`devtriggers`, `devtools`, `threadcheck`) are what turn on the dev surface in every
layer crate, and cargo only unifies features across the packages it was ASKED to build. A bare
`cargo test -p plx_plex` therefore builds a different `plx_plex` (458 tests instead of 466 on
2026-10-05: the `devtriggers` ones are missing) and a different `plx_base` under it, which
neither grades the same code nor shares a single compiled artifact with `make test-fast`. So the
full suite's resolution is read first (`--unit-graph` on the whole `--suite`: nothing is
compiled), and for every workspace crate the selected packages depend on, each feature the full
build enables on it is requested explicitly (`-p plx_ui --features plx_base/devtriggers,...`).
That reproduces the full build's per-crate feature sets, so the fingerprints are the ones
`make test-fast` already wrote: a warm `make test-fast` tree answers `make test-crate` without
recompiling a crate, and the reverse. `ci/test_test_crate.py` pins the command this prints for a
fixture workspace.
"""
from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
APP = "plxnative-modules"
APP_ALIASES = {"app", "plxnative-modules", "plxnative_modules", "modules"}


def run_json(argv: list[str], what: str):
    proc = subprocess.run(argv, stdout=subprocess.PIPE, text=True)
    if proc.returncode != 0:
        raise SystemExit(f"test-crate: {what} failed (exit {proc.returncode}): {' '.join(argv)}")
    return json.loads(proc.stdout)


def pkg_name(pkg_id: str) -> str:
    """`path+file:///…/rust-modules/base#plx_base@0.0.0` -> plx_base (also the older format)."""
    if "#" in pkg_id:
        tail = pkg_id.rsplit("#", 1)[1]
        return tail.rsplit("@", 1)[0] if "@" in tail else tail
    return pkg_id.split()[0]


def resolve(name: str, suite: list[str]) -> str:
    if name in APP_ALIASES and APP in suite:
        return APP
    for cand in (name, "plx_" + name, name.replace("-", "_")):
        if cand in suite:
            return cand
    raise SystemExit(f"test-crate: no crate `{name}` in the unit suite; crates: {', '.join(sorted(suite))}"
                     " (the application crate is `app`)")


def direct_dependents(metadata: dict, suite: list[str], target: str) -> list[str]:
    out = []
    for pkg in metadata["packages"]:
        if pkg["name"] == target or pkg["name"] not in suite:
            continue
        if any(dep["name"] == target for dep in pkg["dependencies"]):
            out.append(pkg["name"])
    return out


def direct_names(metadata: dict, packages: list[str]) -> set[str]:
    """The selected packages and the crates they name in Cargo.toml (normal, dev and build)."""
    names = set(packages)
    for pkg in metadata["packages"]:
        if pkg["name"] in packages:
            names.update(dep["name"] for dep in pkg["dependencies"])
    return names


def feature_flags(graph: dict, wanted: set[str]) -> list[str]:
    """`crate/feature` for every feature the full build enables on `wanted`.

    `wanted` is the selection plus the crates its Cargo.toml names, because that is all cargo lets
    `--features` address ("package X does not contain this feature" for anything further out).
    Everything beyond that is reached through the features the layer crates forward to each other
    (`devtriggers` and `threadcheck` do exactly that). Third-party crates are included: the
    workspace crates ask for different features of the same dependency (rcgen, rustls-pki-types),
    and a build that unifies only the selected few compiles a differently-featured copy of it, and
    everything above it. A name at two versions cannot be addressed as `name/feature`; those are
    left to cargo.
    """
    versions: dict[str, set[str]] = {}
    for unit in graph["units"]:
        versions.setdefault(pkg_name(unit["pkg_id"]), set()).add(unit["pkg_id"])
    features: dict[str, set[str]] = {}
    for unit in graph["units"]:
        name = pkg_name(unit["pkg_id"])
        if name in wanted and len(versions[name]) == 1:
            features.setdefault(name, set()).update(unit.get("features", []))
    return sorted(f"{name}/{feat}" for name, feats in features.items() for feat in feats)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--suite", required=True, help="space-separated package names of the unit suite")
    ap.add_argument("--cargo", default="cargo", help="cargo and its +toolchain, as one string")
    ap.add_argument("--deps", action="store_true", help="also test the crates that directly depend on CRATE")
    ap.add_argument("--filter", default="", help="test-name filter")
    ap.add_argument("--dry-run", action="store_true", help="print the command instead of running it")
    ap.add_argument("crates", nargs="+")
    args = ap.parse_args()

    cargo = shlex.split(args.cargo)
    suite = args.suite.split()
    selected = [resolve(c, suite) for c in args.crates]

    metadata = run_json([*cargo, "metadata", "--format-version", "1", "--no-deps"], "cargo metadata")
    packages = list(dict.fromkeys(selected))
    if args.deps:
        for crate in selected:
            for dep in direct_dependents(metadata, suite, crate):
                if dep not in packages:
                    packages.append(dep)

    full = run_json([*cargo, "test", "--lib", "--no-run", "--unit-graph", "-Z", "unstable-options",
                     *[a for p in suite for a in ("-p", p)]], "cargo --unit-graph")
    flags = feature_flags(full, direct_names(metadata, packages))
    # The application crate is the one that sets the default features; selecting it already
    # resolves them the way the full build does, and naming its own features again is redundant.
    command = [*cargo, "test", "--lib", *[a for p in packages for a in ("-p", p)]]
    if APP not in packages and flags:
        command += ["--features", ",".join(flags)]
    runner = [sys.executable, os.path.join(HERE, "cargo-test-parallel.py")]
    if args.filter:
        runner += ["--filter", args.filter]
    full_command = [*runner, "--", *command]
    if args.dry_run:
        print(" ".join(shlex.quote(a) for a in full_command))
        return 0
    print(f"test-crate: {', '.join(packages)}", file=sys.stderr, flush=True)
    return subprocess.call(full_command)


if __name__ == "__main__":
    sys.exit(main())
