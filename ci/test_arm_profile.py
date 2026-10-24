#!/usr/bin/env python3
"""Which cargo profile the ARM build uses (the Makefile's ARM_PROFILE), and that CI cannot get the fast one.

The ordinary local debug-flavour build compiles the ARM staticlib with `tvdev` (no LTO, 16 codegen
units) so an edit-rebuild is not paid at the fat-LTO price; everything that ships or is graded keeps
`release`. This pins the selection rule through `make -s print-bench-config`, which prints the
resolved value and touches nothing (no stamp, no deletion), and the manifest facts it rests on:

* a plain build selects `tvdev`; RELEASE=1, SYMBOLS=1, FLAVOR=stable|nightly, and a CI environment
  (`CI` / `GITHUB_ACTIONS`, which GitHub sets on every job) select `release`;
* `ARM_PROFILE=` on the command line or in the environment overrides every one of those, in both
  directions, and an unknown value is refused;
* the staticlib path the Makefile links is under the selected profile's directory;
* `[profile.release]` is still fat LTO with one codegen unit (the size budget depends on it) and
  `[profile.tvdev]` inherits it and turns LTO off;
* no workflow names `tvdev`, so a CI job can only reach it by the rule above;
* the other half of the stamp: a goal that only drives the television (`make kill`, which
  `tools/tv-session.sh` runs without an `ARM_PROFILE`) must not reset it. A release-built
  `pkg/plxnative` has the stamp `features:...` while a plain `make` computes `+profile:tvdev`, and the
  Makefile deletes the binary at PARSE time whenever the two differ and the goals are not all in
  `SIDE_EFFECT_FREE`. That is exercised for real, in a scratch tree, under `make -n` (the reset runs
  at parse time, so a dry run reaches it and no recipe, hence no ssh, ever runs).
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRUBBED = ("MAKEFLAGS", "MFLAGS", "MAKELEVEL", "RELEASE", "SYMBOLS", "LAB", "FLAVOR", "CI",
            "GITHUB_ACTIONS", "ARM_PROFILE", "TV")


def resolved(*args: str, env: dict[str, str] | None = None) -> dict[str, str]:
    full = {k: v for k, v in os.environ.items() if k not in SCRUBBED}
    full.update(env or {})
    proc = subprocess.run(["make", "-s", "print-bench-config", *args], cwd=ROOT, capture_output=True,
                          text=True, env=full)
    if proc.returncode != 0:
        raise AssertionError(proc.stderr)
    return dict(line.split("=", 1) for line in proc.stdout.splitlines() if "=" in line)


class ArmProfileSelection(unittest.TestCase):
    def profile(self, *args, env=None):
        return resolved(*args, env=env)["ARM_PROFILE"]

    def test_plain_local_build_is_fast(self):
        cfg = resolved()
        self.assertEqual(cfg["ARM_PROFILE"], "tvdev")
        self.assertEqual(cfg["ARM_PROFILE_FLAG"], "--profile tvdev")
        self.assertTrue(cfg["RUST_LIB"].endswith("/armv7-unknown-linux-gnueabi/tvdev/libplxnative_modules.a"),
                        cfg["RUST_LIB"])
        self.assertEqual(self.profile("FLAVOR=debug", "LAB=1"), "tvdev")

    def test_everything_that_ships_or_is_graded_keeps_the_lto_profile(self):
        for what, args, env in (("RELEASE=1", ["RELEASE=1"], None),
                                ("SYMBOLS=1", ["SYMBOLS=1"], None),
                                ("stable", ["FLAVOR=stable"], None),
                                ("nightly", ["FLAVOR=nightly"], None),
                                ("stable release", ["FLAVOR=stable", "RELEASE=1"], None),
                                ("CI=true", [], {"CI": "true"}),
                                ("GITHUB_ACTIONS=true", [], {"GITHUB_ACTIONS": "true"}),
                                ("RELEASE=1 in the environment", [], {"RELEASE": "1"})):
            with self.subTest(what):
                cfg = resolved(*args, env=env)
                self.assertEqual(cfg["ARM_PROFILE"], "release")
                self.assertEqual(cfg["ARM_PROFILE_FLAG"], "--release")
                self.assertTrue(cfg["RUST_LIB"].endswith("/armv7-unknown-linux-gnueabi/release/libplxnative_modules.a"),
                                cfg["RUST_LIB"])

    def test_the_override_wins_in_both_directions(self):
        self.assertEqual(self.profile("ARM_PROFILE=release"), "release")
        self.assertEqual(self.profile(env={"ARM_PROFILE": "release"}), "release")
        self.assertEqual(self.profile("ARM_PROFILE=tvdev", "RELEASE=1"), "tvdev")
        self.assertEqual(self.profile("ARM_PROFILE=tvdev", env={"CI": "true"}), "tvdev")

    def test_an_unknown_profile_is_refused(self):
        with self.assertRaises(AssertionError) as cm:
            resolved("ARM_PROFILE=fast")
        self.assertIn("unknown ARM_PROFILE", str(cm.exception))

    def test_the_profile_is_part_of_the_stamp_only_when_it_is_not_release(self):
        makefile = (ROOT / "Makefile").read_text()
        m = re.search(r"^RUST_CFG +=(.*)$", makefile, re.M)
        self.assertIsNotNone(m)
        self.assertIn("$(if $(filter-out release,$(ARM_PROFILE)),+profile:$(ARM_PROFILE),)", m.group(1))


# The goals that only DRIVE the television: each has `tv-lock-require` as its one prerequisite and an
# ssh/scp recipe, so none builds or consumes anything the stamp guards. `tools/tv-session.sh` runs
# `kill`, `tests/run.py` runs `kill`, `run` and `run-stream`, and the rest are their siblings.
DEVICE_CONTROL_GOALS = ("kill", "run", "run-stream", "verify-deploy", "uninstall", "fetch-profile",
                        "softfloat-probe")
# What the Makefile's parse-time reset `rm -f`s when the configuration changed, plus the stamp itself.
RESET_PATHS = ("pkg/plxnative", "pkg/plxnative-storage", "pkg/libavformat-plx.so.58", "pkg/.ffabi-ok",
               "vendor/ffmpeg-prefix/include/libavformat/avformat.h")
STAMP = "pkg/.build-config"
PLACEHOLDER_TV = "198.51.100.7"  # TEST-NET-2: a documentation address, never a television


class DeviceControlGoalsKeepTheBuild(unittest.TestCase):
    """`tools/tv-session.sh` closes the app with a plain `make kill` right after a
    `make ARM_PROFILE=release` build. `kill` builds nothing, but it was not in `SIDE_EFFECT_FREE`, so
    its (tvdev) configuration differed from the release stamp and the Makefile deleted `pkg/plxnative`
    and rewrote the stamp: the next `up` died on "no pkg/plxnative -- run make".

    The scratch tree holds symlinks to the checkout's own files (the Makefile reads `ci/` and `tools/`
    while it parses) and REAL `pkg/` and `vendor/` directories. That is load-bearing: the reset removes
    paths under exactly those two, and through a symlink it would delete the checkout's binary and its
    FFmpeg header sentinel (a ~2 minute rebuild).
    """

    def setUp(self):
        td = tempfile.TemporaryDirectory(prefix="plx-stamp.")
        self.addCleanup(td.cleanup)
        self.tree = Path(td.name)
        for entry in ROOT.iterdir():
            if entry.name not in (".git", "pkg", "vendor"):
                (self.tree / entry.name).symlink_to(entry)
        self.env = {k: v for k, v in os.environ.items() if k not in SCRUBBED}

    def make(self, *args: str) -> subprocess.CompletedProcess:
        # `-n`: the reset happens while the Makefile is PARSED, so a dry run reaches it, and no recipe
        # runs. TV is a placeholder as well, so nothing could reach a real set even if one did.
        return subprocess.run(["make", "-n", "--no-print-directory", "-C", str(self.tree), *args],
                              capture_output=True, text=True, env=self.env, timeout=120)

    def seed_release_build(self) -> str:
        """Leave the tree as `make ARM_PROFILE=release` leaves it: the release stamp, and its outputs."""
        proc = self.make("ARM_PROFILE=release", "clean")  # `clean` is not exempt, so this stamps
        self.assertEqual(proc.returncode, 0, proc.stderr[-300:])
        stamp = (self.tree / STAMP).read_text()
        self.assertNotIn("+profile:", stamp, "the release stamp must not carry a profile term")
        for rel in RESET_PATHS:
            path = self.tree / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("built by ARM_PROFILE=release\n")
        return stamp

    def run_goal(self, goal: str, seeded_stamp: str):
        """Run `make <goal>` with the default (tvdev) configuration; report what the reset took."""
        proc = self.make(f"TV={PLACEHOLDER_TV}", "FLAVOR=debug", goal)
        self.assertEqual(proc.returncode, 0, (goal, proc.stderr[-300:]))
        deleted = [rel for rel in RESET_PATHS if not (self.tree / rel).exists()]
        stamp = (self.tree / STAMP).read_text()
        return deleted, stamp != seeded_stamp, "+profile:tvdev" in stamp

    def test_goals_that_only_drive_the_tv_keep_a_release_build(self):
        for goal in ("print-appid",) + DEVICE_CONTROL_GOALS:  # print-appid: an exempt goal that must pass today
            with self.subTest(goal):
                seeded = self.seed_release_build()
                deleted, stamp_changed, stamp_is_tvdev = self.run_goal(goal, seeded)
                self.assertEqual(deleted, [], f"make {goal} deleted a release build's files")
                self.assertFalse(stamp_changed, f"make {goal} rewrote the release stamp"
                                                f"{' to the tvdev configuration' if stamp_is_tvdev else ''}")

    def test_a_goal_that_builds_still_resets_the_stamp(self):
        """The control: without it a parse that never reached the reset would pass the test above."""
        seeded = self.seed_release_build()
        deleted, stamp_changed, stamp_is_tvdev = self.run_goal("clean", seeded)
        self.assertEqual(deleted, list(RESET_PATHS))
        self.assertTrue(stamp_changed and stamp_is_tvdev)


def profile_table(name: str) -> dict[str, str]:
    """The `key = value` lines of `[profile.<name>]` in rust-modules/Cargo.toml, values as raw text.

    A regex rather than a TOML parser: `tomllib` is 3.11+ and the host python this runs under (the
    macOS system one) is older. Comment lines and blanks are skipped; the table ends at the next `[`.
    """
    text = (ROOT / "rust-modules" / "Cargo.toml").read_text()
    m = re.search(r"^\[profile\." + re.escape(name) + r"\]\n((?:(?!\[).*\n)*)", text, re.M)
    if not m:
        raise AssertionError(f"no [profile.{name}] in rust-modules/Cargo.toml")
    out = {}
    for line in m.group(1).splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            key, _, value = line.partition("=")
            out[key.strip()] = value.strip()
    return out


class ManifestFacts(unittest.TestCase):
    def test_release_is_still_the_size_budget_profile(self):
        self.assertEqual(profile_table("release"),
                         {"opt-level": "2", "lto": '"fat"', "codegen-units": "1"})

    def test_tvdev_differs_from_release_only_in_lto_and_codegen_units(self):
        fast = profile_table("tvdev")
        self.assertEqual(fast["inherits"], '"release"')
        self.assertEqual(fast["lto"], '"off"')
        self.assertGreater(int(fast["codegen-units"]), 1)
        self.assertEqual(set(fast) - {"inherits", "lto", "codegen-units"}, set(),
                         "a tvdev override beyond LTO/codegen units changes what is being compared with release")


class WorkflowsNeverNameTheFastProfile(unittest.TestCase):
    def test_no_workflow_mentions_it(self):
        # Workflows and composite actions only: ci.yml's `paths` filter skips pushes that touch only
        # the rest of .github/ (issue forms, FUNDING.yml), which are not workflows.
        for wf in sorted([*(ROOT / ".github/workflows").glob("*.yml"), *(ROOT / ".github/actions").glob("*/action.yml")]):
            with self.subTest(wf.relative_to(ROOT).as_posix()):
                self.assertNotIn("tvdev", wf.read_text())


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)
