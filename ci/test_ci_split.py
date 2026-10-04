#!/usr/bin/env python3
"""The host gates run as parallel CI jobs; this pins that the split lost no gate.

`make check` is `check-cargo` (lint, default unit pass, hostsim unit pass, the ci/ self-tests that
drive cargo) plus `check-python`. CI used to run it as one job and now runs the pieces as four
jobs, so a gate can be dropped in exactly two new ways: it ends up in no cargo target, or CI stops
calling the target it is in. Both are silent (everything stays green), so both are pinned here:

* the Makefile's `check-cargo` is the serial union of the three `check-cargo-*` targets and adds no
  gate of its own;
* each cargo gate lives in exactly ONE of the three, and in the one the CI job for it runs;
* ci.yml has a job per target, an aggregator under the old job name that needs all of them and
  does not skip when one fails, and SDL only where a test binary links.

Text-level on purpose (no YAML library on a stock runner): the files are ours and regular.
"""
from __future__ import annotations

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAKEFILE = (ROOT / "Makefile").read_text().splitlines()
CI = (ROOT / ".github/workflows/ci.yml").read_text()

CARGO_TARGETS = ["check-cargo-lint", "check-cargo-unit-default", "check-cargo-unit-hostsim"]
# job id -> the make target it must run
JOBS = {
    "host-lint": "check-cargo-lint",
    "host-unit-default": "check-cargo-unit-default",
    "host-unit-hostsim": "check-cargo-unit-hostsim",
    "host-python": "check-python",
}
AGGREGATOR_NAME = "host checks (NOT a device gate)"


def rule(target):
    """(prerequisites, recipe lines without comments) of a Makefile target."""
    start = next(i for i, line in enumerate(MAKEFILE) if line.startswith(target + ":"))
    prereqs = MAKEFILE[start].split(":", 1)[1].split()
    recipe = []
    for line in MAKEFILE[start + 1:]:
        if line and not line.startswith(("\t", "#")):
            break
        if line.startswith("\t") and not line.lstrip("\t").startswith("@#"):
            recipe.append(line)
    return prereqs, "\n".join(recipe)


# Each cargo gate, as a regex over a target's whole (comment-free) recipe, and the one target that
# owns it. `lint` is a prerequisite rather than a recipe line, handled separately.
GATES = {
    "check-cargo-lint": [r"cargo \+\$\(RUST_NIGHTLY\) check --lib --tests -p plxnative-modules -p plx_base -p plx_machine -p plx_platform -p plx_gfx -p plx_net -p plx_ui --features lab-diagnostics"],
    "check-cargo-unit-default": [
        r"cargo \+\$\(RUST_NIGHTLY\) test --lib -p plxnative-modules -p plx_base -p plx_machine -p plx_platform -p plx_gfx -p plx_net -p plx_ui\n",
        r"ci/test_storage_service_package\.py",
        r"test -p plxnative-storage --bin plxnative-storage",
        r"ci/test_storage_package_isolated\.py",
        r"ci/test_no_host_staticlib\.py",
        r"ci/test_build_not_always_dirty\.py",
    ],
    "check-cargo-unit-hostsim": [r"cargo \+\$\(RUST_NIGHTLY\) test --lib -p plxnative-modules -p plx_base -p plx_machine -p plx_platform -p plx_gfx -p plx_net -p plx_ui --features hostsim"],
}


def job_blocks():
    jobs_at = CI.index("\njobs:\n")
    text = CI[jobs_at:]
    parts = re.split(r"^  ([a-z][a-z0-9-]*):\n", text, flags=re.M)
    return dict(zip(parts[1::2], parts[2::2]))


class CheckCargoIsTheUnion(unittest.TestCase):
    def test_check_cargo_runs_the_three_targets_in_order_and_nothing_else(self):
        prereqs, recipe = rule("check-cargo")
        self.assertEqual(prereqs, [])
        called = re.findall(r"check-cargo-[a-z-]+", recipe)
        self.assertEqual(called, CARGO_TARGETS)
        # nothing but those three sub-make lines: a gate added HERE would run locally and in no CI job
        self.assertEqual(len(recipe.splitlines()), len(CARGO_TARGETS), recipe)

    def test_lint_is_a_prerequisite_of_the_lint_target_only(self):
        for target in CARGO_TARGETS:
            prereqs, _ = rule(target)
            self.assertEqual(prereqs, ["lint"] if target == "check-cargo-lint" else [], target)

    def test_each_cargo_gate_lives_in_exactly_one_target(self):
        recipes = {t: rule(t)[1] + "\n" for t in CARGO_TARGETS}
        for owner, patterns in GATES.items():
            for pattern in patterns:
                holders = [t for t, recipe in recipes.items() if re.search(pattern, recipe)]
                self.assertEqual(holders, [owner], f"{pattern!r} must be run by {owner} alone")

    def test_the_unlocked_branches_are_unchanged(self):
        _, recipe = rule("check-unlocked")
        self.assertIn("check-cargo", recipe)
        self.assertIn("check-python", recipe)


class CiRunsEveryTarget(unittest.TestCase):
    def test_each_job_runs_its_target(self):
        blocks = job_blocks()
        for job, target in JOBS.items():
            self.assertIn(job, blocks, f"ci.yml lost the {job} job")
            self.assertRegex(blocks[job], rf"make {re.escape(target)}(?![\w-])",
                             f"{job} must run `make {target}`")

    def test_every_target_is_run_by_exactly_one_job(self):
        blocks = job_blocks()
        for target in [*CARGO_TARGETS, "check-python"]:
            runners = [job for job, block in blocks.items()
                       if re.search(rf"make {re.escape(target)}(?![\w-])", block)]
            self.assertEqual(len(runners), 1, f"{target} runs in {runners}")

    def test_aggregator_keeps_the_old_name_and_cannot_be_skipped_green(self):
        block = job_blocks()["host-checks"]
        self.assertIn(f"name: {AGGREGATOR_NAME}", block)
        needs = re.search(r"^    needs: \[([^\]]*)\]", block, flags=re.M)
        self.assertIsNotNone(needs)
        self.assertEqual(sorted(n.strip() for n in needs.group(1).split(",")), sorted(JOBS))
        self.assertRegex(block, re.compile(r"^    if: always\(\)", re.M), "a job that needs a failed job is skipped, and a skip is not red")
        for job in JOBS:
            self.assertIn(f"needs.{job}.result", block)
        self.assertIn("exit 1", block)

    def test_sdl_is_installed_only_where_a_test_binary_links(self):
        blocks = job_blocks()
        for job in ("host-unit-default", "host-unit-hostsim"):
            self.assertRegex(blocks[job], r'sdl: "true"', f"{job} links a host test binary")
        for job in ("host-lint", "host-python"):
            self.assertNotIn("sdl:", blocks[job], f"{job} links nothing; the apt step is minutes on a noisy runner")

    def test_one_job_name_per_job(self):
        names = re.findall(r"^    name: (.*)$", CI, flags=re.M)
        self.assertEqual(len(names), len(set(names)), names)


if __name__ == "__main__":
    unittest.main()
