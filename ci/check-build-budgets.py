#!/usr/bin/env python3
"""Deterministic build-health budgets: fail CI when the build grows past a number a reviewer set.

usage: check-build-budgets.py [--budgets ci/build-budgets.json]
           [--binary PATH]                  stripped ARM binary (cross-build job)
           [--graph]                        run `cargo tree` for both shipping feature sets
           [--tree-default FILE --tree-no-default FILE]   canned {"all": ..., "normal": ...} listings (tests)
           [--src rust-modules/src ...]       count Rust source lines of each --src (informational budget)

Every budget lives in ci/build-budgets.json with the value it was set from and the date, so a
reader can see how much headroom it carries. Nothing here is a timing: wall-clock numbers are noisy
and live in tools/ci-durations.py; these are counts and byte sizes that are identical on every run
of the same tree, so a red result is always a real change to the tree.

* binary_bytes                 the stripped binary `make ipk` stages (measured in the cross-build job)
* packages_*                   third-party packages the app crate compiles for the ARM target
* duplicate_versions_*         crate names in more than one version on the normal-edge graph
* source_lines                 lines of Rust in rust-modules/src and every split-out layer crate (rust-modules/base/src, rust-modules/machine/src, rust-modules/net/src, rust-modules/platform/src, rust-modules/gfx/src, rust-modules/ui/src); mode "warn", never fails

Exit status: 0 all within budget (warn-mode overruns print ::warning::), 1 a fail-mode budget is
exceeded, 2 the budgets file or an input is unusable.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BUDGETS = ROOT / "ci" / "build-budgets.json"
TARGET = "arm-unknown-linux-gnueabi"  # the Makefile's RUST_TARGET
REQUIRED_FIELDS = ("what", "unit", "mode", "limit", "measured", "measured_on", "headroom")
MODES = ("fail", "warn")
KNOWN = (
    "binary_bytes",
    "packages_default",
    "packages_no_default_features",
    "duplicate_versions_default",
    "duplicate_versions_no_default_features",
    "source_lines",
)

HOW_TO_RAISE = (
    "To raise this budget deliberately: edit ci/build-budgets.json in the SAME pull request, set "
    "`limit` to the new ceiling, update `measured` and `measured_on`, and justify the growth in the "
    "PR body (what was added, why it cannot be smaller). Do not raise it just to make a run green."
)


class BudgetError(Exception):
    """The budgets file or an input is unusable (exit 2)."""


def load_budgets(path: Path) -> dict:
    try:
        doc = json.loads(Path(path).read_text())
    except (OSError, ValueError) as exc:
        raise BudgetError(f"cannot read budgets file {path}: {exc}") from exc
    problems = schema_problems(doc)
    if problems:
        raise BudgetError(f"{path}: " + "; ".join(problems))
    return doc["budgets"]


def schema_problems(doc) -> list[str]:
    if not isinstance(doc, dict):
        return ["top level must be an object"]
    problems = []
    if doc.get("schema") != 1:
        problems.append("`schema` must be 1")
    if not isinstance(doc.get("how_to_raise"), str) or not doc["how_to_raise"]:
        problems.append("`how_to_raise` must be a non-empty string")
    budgets = doc.get("budgets")
    if not isinstance(budgets, dict):
        return problems + ["`budgets` must be an object"]
    for name in KNOWN:
        if name not in budgets:
            problems.append(f"budget {name!r} is missing")
    for name, b in budgets.items():
        if name not in KNOWN:
            problems.append(f"unknown budget {name!r}")
            continue
        if not isinstance(b, dict):
            problems.append(f"budget {name!r} must be an object")
            continue
        for field in REQUIRED_FIELDS:
            if field not in b:
                problems.append(f"budget {name!r} lacks `{field}`")
        if b.get("mode") not in MODES:
            problems.append(f"budget {name!r} mode must be one of {MODES}")
        for field in ("limit", "measured"):
            v = b.get(field)
            if not isinstance(v, int) or isinstance(v, bool) or v < 0:
                problems.append(f"budget {name!r} `{field}` must be a non-negative integer")
        if isinstance(b.get("limit"), int) and isinstance(b.get("measured"), int) and b["limit"] < b["measured"]:
            problems.append(f"budget {name!r} limit is below the value it was measured at")
    return problems


TREE_LINE = re.compile(r"^(\S+) v(\S+)(.*)$")


def tree_packages(tree: str, workspace: Path) -> set[tuple[str, str]]:
    """(name, version) of every third-party package a `cargo tree --prefix none` listing names.

    Third-party means not built from a path inside `workspace`: the crates of this repository's own
    layer split are path packages under rust-modules/ and are not counted, however many of them
    there are. `cargo tree` prints `name vX.Y.Z`, then optional `(/abs/path)`, `(proc-macro)` and
    `(*)` (already printed) markers.
    """
    root = workspace.resolve()
    found = set()
    for line in tree.splitlines():
        m = TREE_LINE.match(line.strip())
        if not m:
            continue
        marks = re.findall(r"\(([^)]*)\)", m.group(3))
        local = any(os.path.isabs(x) and Path(x).resolve().is_relative_to(root) for x in marks)
        if not local:
            found.add((m.group(1), m.group(2)))
    return found


def graph_metrics(trees: dict, workspace: Path) -> dict[str, int]:
    """(third-party packages, duplicate crate names) for the graph the SHIPPING build resolves.

    `trees["all"]` is `cargo tree -e normal,build` (what gets compiled for the target build,
    dev-dependencies excluded) and `trees["normal"]` is `cargo tree -e normal`, the set whose
    duplicates `cargo tree -d --edges normal` prints.

    It is `cargo tree` and not `cargo metadata` on purpose: metadata unifies features across every
    dependency kind, so a layer crate's `test-support` feature (which the app crate switches on in
    its `[dev-dependencies]` only) pulls that feature's optional crates (rcgen, rustls, ...) into
    the "resolved graph" of a build that never compiles them. `cargo tree` resolves features the
    way a build does, and a dev-dependency's features stay out of a non-test build.
    """
    for key in ("all", "normal"):
        if not isinstance(trees.get(key), str) or not trees[key].strip():
            raise BudgetError(f"cargo tree listing {key!r} is empty (run it from the app crate)")
    names = Counter(name for name, _ in tree_packages(trees["normal"], workspace))
    return {
        "packages": len(tree_packages(trees["all"], workspace)),
        "duplicates": sum(1 for n in names.values() if n > 1),
    }


def run_cargo_tree(no_default_features: bool) -> dict:
    toolchain = os.environ.get("RUST_NIGHTLY", "nightly")
    return cargo_trees(ROOT / "rust-modules", toolchain, no_default_features)


def cargo_trees(manifest_dir: Path, toolchain: str, no_default_features: bool) -> dict:
    env = dict(os.environ, CARGO_INCREMENTAL="0")
    trees = {}
    for key, edges in (("all", "normal,build"), ("normal", "normal")):
        cmd = ["cargo", f"+{toolchain}", "tree", "--prefix", "none", "--edges", edges,
               "--target", TARGET, "--format", "{p}"]
        if (manifest_dir / "Cargo.lock").exists():
            cmd.append("--locked")  # a drifted lockfile is a failure, never a silent re-resolve
        if no_default_features:
            cmd.append("--no-default-features")
        try:
            trees[key] = subprocess.run(cmd, cwd=manifest_dir, env=env, check=True,
                                        capture_output=True, text=True, timeout=300).stdout
        except (OSError, subprocess.SubprocessError) as exc:
            raise BudgetError(f"`{' '.join(cmd)}` failed: {exc}") from exc
    return trees


def source_lines(src: Path) -> int:
    files = sorted(Path(src).rglob("*.rs"))
    if not files:
        raise BudgetError(f"no .rs files under {src}")
    return sum(len(f.read_bytes().splitlines()) for f in files)


def binary_size(path: str) -> int:
    p = Path(path)
    if not p.is_file():
        raise BudgetError(f"binary {path} does not exist (run after `make ipk`)")
    with p.open("rb") as handle:
        if handle.read(4) != b"\x7fELF":
            raise BudgetError(f"{path} is not an ELF file")
    return p.stat().st_size


def evaluate(budgets: dict, measured: dict[str, int]) -> list[dict]:
    rows = []
    for name, value in measured.items():
        b = budgets[name]
        rows.append({
            "name": name, "value": value, "limit": b["limit"], "unit": b["unit"], "mode": b["mode"],
            "baseline": b["measured"], "baseline_on": b["measured_on"],
            "status": "ok" if value <= b["limit"] else ("WARN" if b["mode"] == "warn" else "FAIL"),
        })
    return rows


def render(rows: list[dict]) -> str:
    lines = ["| budget | measured | limit | baseline (date) | status |", "|---|---:|---:|---:|---|"]
    for r in rows:
        lines.append(f"| {r['name']} | {r['value']:,} {r['unit']} | {r['limit']:,} | "
                     f"{r['baseline']:,} ({r['baseline_on']}) | {r['status']} |")
    return "\n".join(lines)


def collect(args, budgets: dict) -> dict[str, int]:
    measured: dict[str, int] = {}
    if args.binary:
        measured["binary_bytes"] = binary_size(args.binary)
    if args.graph or args.tree_default or args.tree_no_default:
        for suffix, canned, no_default in (("default", args.tree_default, False),
                                           ("no_default_features", args.tree_no_default, True)):
            if canned:
                trees = json.loads(Path(canned).read_text())
            elif args.graph:
                trees = run_cargo_tree(no_default)
            else:
                continue
            m = graph_metrics(trees, ROOT / "rust-modules")
            measured[f"packages_{suffix}"] = m["packages"]
            measured[f"duplicate_versions_{suffix}"] = m["duplicates"]
    if args.src:
        measured["source_lines"] = sum(source_lines(Path(s)) for s in args.src)
    return measured


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--budgets", default=str(DEFAULT_BUDGETS))
    ap.add_argument("--binary")
    ap.add_argument("--graph", action="store_true")
    ap.add_argument("--tree-default")
    ap.add_argument("--tree-no-default")
    ap.add_argument("--src", action="append", help="a source directory to count; repeat for each crate (rust-modules/src, rust-modules/base/src, rust-modules/machine/src, rust-modules/net/src, rust-modules/platform/src, rust-modules/gfx/src, rust-modules/ui/src)")
    args = ap.parse_args(argv)
    try:
        budgets = load_budgets(Path(args.budgets))
        measured = collect(args, budgets)
        if not measured:
            raise BudgetError("nothing to measure: pass --binary, --graph and/or --src")
    except BudgetError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    rows = evaluate(budgets, measured)
    table = render(rows)
    print(table)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write("### Build budgets (ci/build-budgets.json)\n" + table + "\n")
    failed = False
    for r in rows:
        if r["status"] == "ok":
            continue
        message = (f"build budget {r['name']} exceeded: {r['value']:,} {r['unit']} > limit "
                   f"{r['limit']:,} (set from {r['baseline']:,} on {r['baseline_on']}).")
        if r["status"] == "WARN":
            print(f"::warning::{message} Informational only.")
        else:
            failed = True
            print(f"::error::{message} {HOW_TO_RAISE}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
