#!/usr/bin/env python3
"""The Makefile's HOST_THREADS: rustc's parallel front end for host test builds, OPT-IN, and nothing else.

The host suites run on the pinned STABLE toolchain, whose rustc rejects every `-Z` flag, so the
default is 0 (the serial front end, the commands CI runs). `make RUST_TOOLCHAIN=nightly
HOST_THREADS=N ...` opts in: the Makefile then passes `-Zthreads=N` to the host `cargo test` recipes
through CARGO_BUILD_RUSTFLAGS, because the crate stack is a chain and the front end is
single-threaded per crate by default (measured in the Makefile's comment). This pins the scoping
rule the way ci/test_arm_profile.py pins ARM_PROFILE's, through `make -s print-bench-config` (which
prints the resolved value and touches nothing) and `make -n` (which prints a recipe without running
cargo):

* every build resolves 0 by default: plain local, RELEASE=1, SYMBOLS=1, FLAVOR=stable|nightly and a
  CI environment alike;
* `HOST_THREADS=` on the command line or in the environment picks the count, but only with a
  nightly toolchain: asking for more than one thread on the stable pin is a Makefile error that
  names the fix, rather than a cargo failure in the middle of a recipe;
* the flag reaches exactly the host test recipes (`check-cargo-unit-default`, `-hostsim`,
  `test-fast`, `test-crate`, `build-bench`) when it is on, and NO recipe when it is off, so a CI
  recipe is the command it always was;
* it is never exported and no ARM, simulator, clippy or package line, and no workflow, names it, so a
  release or simulator build cannot pick it up;
* when a nightly is installed, the pinned spelling is still accepted by it (rustc's own help calls
  `-Zthreads` deprecated in favour of `--jobs-frontend`; an unknown `-Z` option is a hard error, so a
  nightly that drops it must fail here rather than in a developer's opted-in `make check`).
"""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRUBBED = ("MAKEFLAGS", "MFLAGS", "MAKELEVEL", "RELEASE", "SYMBOLS", "LAB", "FLAVOR", "CI",
            "GITHUB_ACTIONS", "ARM_PROFILE", "HOST_THREADS")
FLAG = "CARGO_BUILD_RUSTFLAGS"
HOST_RECIPES = (("check-cargo-unit-default",), ("check-cargo-unit-hostsim",), ("test-fast",),
                ("test-crate", "C=plx_base"), ("build-bench",))


def make(*args: str, env: dict[str, str] | None = None) -> str:
    full = {k: v for k, v in os.environ.items() if k not in SCRUBBED}
    full.update(env or {})
    proc = subprocess.run(["make", *args], cwd=ROOT, capture_output=True, text=True, env=full)
    if proc.returncode != 0:
        raise AssertionError(proc.stderr)
    return proc.stdout


def threads(*args: str, env: dict[str, str] | None = None) -> int:
    cfg = dict(line.split("=", 1) for line in make("-s", "print-bench-config", *args, env=env).splitlines()
               if "=" in line)
    return int(cfg["HOST_THREADS"])


def recipe(goal: tuple[str, ...], *args: str, env: dict[str, str] | None = None) -> str:
    return make("-n", *goal, *args, env=env)


NIGHTLY = "RUST_TOOLCHAIN=nightly"


class Resolution(unittest.TestCase):
    def test_every_build_defaults_to_the_serial_front_end(self):
        for what, args, env in (("plain", [], None),
                                ("debug+lab", ["FLAVOR=debug", "LAB=1"], None),
                                ("RELEASE=1", ["RELEASE=1"], None),
                                ("SYMBOLS=1", ["SYMBOLS=1"], None),
                                ("stable", ["FLAVOR=stable"], None),
                                ("nightly", ["FLAVOR=nightly"], None),
                                ("CI=true", [], {"CI": "true"}),
                                ("GITHUB_ACTIONS=true", [], {"GITHUB_ACTIONS": "true"}),
                                ("RELEASE=1 in the environment", [], {"RELEASE": "1"})):
            with self.subTest(what):
                self.assertEqual(threads(*args, env=env), 0)

    def test_the_override_picks_the_count_on_a_nightly(self):
        self.assertEqual(threads("HOST_THREADS=0", NIGHTLY), 0)
        self.assertEqual(threads(NIGHTLY, env={"HOST_THREADS": "0"}), 0)
        self.assertEqual(threads("HOST_THREADS=4", NIGHTLY), 4)
        self.assertEqual(threads("HOST_THREADS=3", "RELEASE=1", NIGHTLY), 3)
        self.assertEqual(threads(NIGHTLY, env={"CI": "true", "HOST_THREADS": "3"}), 3)

    def test_asking_for_threads_on_the_stable_pin_is_an_error_that_names_the_fix(self):
        proc = subprocess.run(["make", "-s", "print-bench-config", "HOST_THREADS=4"], cwd=ROOT, capture_output=True,
                              text=True, env={k: v for k, v in os.environ.items() if k not in SCRUBBED})
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("RUST_TOOLCHAIN=nightly", proc.stderr)
        # one thread is the serial front end, which every toolchain accepts
        self.assertEqual(threads("HOST_THREADS=1"), 1)


class Recipes(unittest.TestCase):
    def test_the_flag_reaches_exactly_the_host_test_recipes(self):
        for goal in HOST_RECIPES:
            with self.subTest(goal[0]):
                self.assertIn(f"{FLAG}='-Zthreads=4'", recipe(goal, "HOST_THREADS=4", NIGHTLY))

    def test_off_means_no_recipe_names_it(self):
        # One `make -n` per condition, naming every goal it applies to: each parse of the Makefile is
        # ~0.5 s and this used to run one per goal and condition (30). `-n` prints each goal's recipe
        # once, so the output holds exactly the lines the per-goal runs held.
        for what, args, env in (("default", [], None), ("override", ["HOST_THREADS=0"], None),
                                ("one is serial", ["HOST_THREADS=1"], None), ("CI", [], {"CI": "true"}), ("GITHUB_ACTIONS", [], {"GITHUB_ACTIONS": "true"}),
                                ("RELEASE", ["RELEASE=1"], None), ("stable", ["FLAVOR=stable"], None)):
            # those goals refuse RELEASE=1 outright
            goals = [goal for goal in HOST_RECIPES
                     if not (goal[0] in ("test-fast", "test-crate", "build-bench") and what in ("RELEASE",))]
            with self.subTest(f"{what}: {', '.join(goal[0] for goal in goals)}"):
                out = make("-n", *[word for goal in goals for word in goal], *args, env=env)
                self.assertNotIn(FLAG, out)
                self.assertNotIn("Zthreads", out)

    def test_the_arm_and_simulator_recipes_never_name_it(self):
        text = (ROOT / "Makefile").read_text()
        self.assertNotRegex(text, r"^\s*export\s+(HOST_ENV|HOST_THREADS|CARGO_BUILD_RUSTFLAGS)\b", "it must not be exported")
        for lineno, line in enumerate(text.splitlines(), 1):
            if "$(HOST_ENV)" not in line or line.lstrip().startswith("#"):
                continue
            for banned in ("--target $(RUST_TARGET)", "$(RUST_ENV)", "--release", "cargo build", "clippy", "$(ARM_PROFILE"):
                self.assertNotIn(banned, line, f"Makefile:{lineno} puts the host flag on a non-host line")
        # ...and the lines that DO carry it are the host test recipes, nothing else.
        users = [l for l in text.splitlines() if "$(HOST_ENV)" in l and not l.lstrip().startswith("#")]
        self.assertEqual(len(users), len(HOST_RECIPES), users)

    def test_no_workflow_names_it(self):
        # Workflows and composite actions only: ci.yml's `paths` filter skips pushes that touch only
        # the rest of .github/ (issue forms, FUNDING.yml), which are not workflows.
        for wf in sorted([*(ROOT / ".github/workflows").glob("*.yml"), *(ROOT / ".github/actions").glob("*/action.yml")]):
            with self.subTest(wf.relative_to(ROOT).as_posix()):
                self.assertNotRegex(wf.read_text(), r"HOST_THREADS|Zthreads|CARGO_BUILD_RUSTFLAGS")

    def test_print_bench_config_reports_it(self):
        self.assertIn("HOST_THREADS=", make("-s", "print-bench-config"))


class Toolchain(unittest.TestCase):
    def test_the_pinned_spelling_is_still_accepted_by_a_nightly(self):
        if not (shutil.which("rustc") or (Path.home() / ".cargo/bin/rustc").exists()):
            self.skipTest("no rustc")
        env = dict(os.environ, PATH=f"{Path.home()}/.cargo/bin:{os.environ.get('PATH', '')}")
        probe = subprocess.run(["rustc", "+nightly", "-Zthreads=2", "--print", "sysroot"], capture_output=True,
                               text=True, env=env)
        if "toolchain" in probe.stderr and "is not installed" in probe.stderr:
            self.skipTest("no nightly toolchain installed here (it is opt-in, not required)")
        self.assertEqual(probe.returncode, 0, probe.stderr)
        bad = subprocess.run(["rustc", "+nightly", "-Zthreads_definitely_not_an_option=2", "--print", "sysroot"],
                             capture_output=True, text=True, env=env)
        self.assertNotEqual(bad.returncode, 0, "an unknown -Z option no longer fails, so the check above proves nothing")


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)
