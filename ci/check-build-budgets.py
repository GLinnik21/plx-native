#!/usr/bin/env python3
"""Deterministic build-health budgets: fail CI when the build grows past a number a reviewer set.

usage: check-build-budgets.py [--budgets ci/build-budgets.json]
           [--binary PATH]                  stripped ARM binary (cross-build job)
           [--graph]                        run `cargo metadata` for both shipping feature sets
           [--metadata-default FILE --metadata-no-default FILE]   canned metadata (tests)
           [--src rust-modules/src]         count Rust source lines (informational budget)

Every budget lives in ci/build-budgets.json with the value it was set from and the date, so a
reader can see how much headroom it carries. Nothing here is a timing: wall-clock numbers are noisy
and live in tools/ci-durations.py; these are counts and byte sizes that are identical on every run
of the same tree, so a red result is always a real change to the tree.

* binary_bytes                 the stripped binary `make ipk` stages (measured in the cross-build job)
* packages_*                   third-party packages the app crate compiles for the ARM target
* duplicate_versions_*         crate names in more than one version on the normal-edge graph
* source_lines                 lines of Rust in rust-modules/src; mode "warn", never fails

Exit status: 0 all within budget (warn-mode overruns print ::warning::), 1 a fail-mode budget is
exceeded, 2 the budgets file or an input is unusable.
"""
from __future__ import annotations

import argparse
import json
import os
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


def graph_metrics(metadata: dict) -> dict[str, int]:
    """(third-party packages, duplicate crate names) for the root package's resolved graph.

    Packages: everything reachable over normal + build edges (what gets compiled for the shipping
    build), dev-dependencies excluded, workspace members excluded (`source` is null for them).
    Duplicates: crate names present in more than one version over NORMAL edges alone, which is the
    set `cargo tree -d --edges normal` prints.
    """
    resolve = metadata.get("resolve")
    if not resolve or not resolve.get("root"):
        raise BudgetError("cargo metadata has no resolved root package (run it from the app crate)")
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in resolve["nodes"]}

    def reachable(kinds):
        seen, stack = {resolve["root"]}, [resolve["root"]]
        while stack:
            for dep in nodes[stack.pop()]["deps"]:
                if dep["pkg"] in seen:
                    continue
                if any(k.get("kind") in kinds for k in dep.get("dep_kinds", [])):
                    seen.add(dep["pkg"])
                    stack.append(dep["pkg"])
        return {i for i in seen if packages[i].get("source")}

    third_party = reachable({None, "build"})
    normal = reachable({None})
    names = Counter((packages[i]["name"], packages[i]["version"]) for i in normal)
    by_name = Counter(name for name, _ in names)
    return {
        "packages": len(third_party),
        "duplicates": sum(1 for n in by_name.values() if n > 1),
    }


def run_cargo_metadata(no_default_features: bool) -> dict:
    toolchain = os.environ.get("RUST_NIGHTLY", "nightly")
    cmd = ["cargo", f"+{toolchain}", "metadata", "--format-version", "1", "--locked",
           "--filter-platform", TARGET]
    if no_default_features:
        cmd.append("--no-default-features")
    env = dict(os.environ, CARGO_INCREMENTAL="0")
    try:
        out = subprocess.run(cmd, cwd=ROOT / "rust-modules", env=env, check=True,
                             capture_output=True, text=True, timeout=300).stdout
    except (OSError, subprocess.SubprocessError) as exc:
        raise BudgetError(f"`{' '.join(cmd)}` failed: {exc}") from exc
    return json.loads(out)


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
    if args.graph or args.metadata_default or args.metadata_no_default:
        for suffix, canned, no_default in (("default", args.metadata_default, False),
                                           ("no_default_features", args.metadata_no_default, True)):
            if canned:
                metadata = json.loads(Path(canned).read_text())
            elif args.graph:
                metadata = run_cargo_metadata(no_default)
            else:
                continue
            m = graph_metrics(metadata)
            measured[f"packages_{suffix}"] = m["packages"]
            measured[f"duplicate_versions_{suffix}"] = m["duplicates"]
    if args.src:
        measured["source_lines"] = source_lines(Path(args.src))
    return measured


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--budgets", default=str(DEFAULT_BUDGETS))
    ap.add_argument("--binary")
    ap.add_argument("--graph", action="store_true")
    ap.add_argument("--metadata-default")
    ap.add_argument("--metadata-no-default")
    ap.add_argument("--src")
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
