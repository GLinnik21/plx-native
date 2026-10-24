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
* the other half of the stamp: a goal that builds nothing must not reset it. A release-built
  `pkg/plxnative` has the stamp `features:...` while a plain `make` computes `+profile:tvdev`, and the
  Makefile deletes the binary at PARSE time whenever the two differ and the goals are not all in
  `SIDE_EFFECT_FREE`. Two kinds of goal leave the build alone: the ones that only drive the television
  (`make kill`, which `tools/tv-session.sh` runs without an `ARM_PROFILE`) and the host-only ones (the
  unit-test loops, the lint pass, the print queries, the set-up and fixture tooling), e.g. `make
  test-crate` after a release build. Each is exercised for real, in a scratch tree, under `make -n`
  (the reset runs at parse time, so a dry run reaches it and no recipe, hence no ssh, ever runs). The
  goals that DO build, read or ship an ARM artifact or anything under `pkg/` are pinned out of the list
  too, each with its reason, so the list cannot grow past the rule unnoticed.
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
# `kill`, `tests/run.py` runs `kill`, `run` and `run-stream`, and the rest are their siblings. The lock
# check they share is one of them: it is a shell call to tools/tv-lock.sh and nothing else.
DEVICE_CONTROL_GOALS = ("kill", "run", "run-stream", "verify-deploy", "uninstall", "fetch-profile",
                        "softfloat-probe", "tv-lock-require")
# The goals that run only on the HOST and never build, read or ship `pkg/plxnative`, the ARM library or
# anything else under `pkg/` (each recipe and each prerequisite was read, not judged from its name).
HOST_ONLY_GOALS = (
    # the unit-test loops and the lint pass (host cargo in a tree under rust-modules/), and the pieces of
    # `check` that are plain host work: `check-cargo-lint` (clippy and the lab-diagnostics type-check),
    # `check-cargo-unit-hostsim` and `check-python-harness` (tests/test_harness.py alone). `test-hostsim`
    # and `check-nodef` were the first two to be listed.
    "test-hostsim", "check-nodef", "test-fast", "test-crate", "lint", "check-cargo-lint", "check-cargo-unit-hostsim",
    "check-localization", "check-c-unit", "check-python-harness",
    # the documentation tables
    "notices", "notices-verify",
    # set-up and local tooling: a toolchain install, a launchd agent, the test media, the demo library
    # cache and the simulator's own instance root. None of them writes under pkg/.
    "setup-rust", "setup-env", "disk-watch", "disk-watch-uninstall",
    "fixtures", "fixtures-quick", "fixtures-pipeline", "demo-library",
    "sim-token", "sim-macos-token", "sim-clean", "sim-macos-clean",
    # print queries that were missing from QUERY_GOALS: `make -s print-rust-toolchain` is what a script
    # runs to learn the pin, and it deleted a release build as readily as a compile did
    "print-subs-font-files", "print-rust-toolchain", "print-rust-codegen", "print-cargo-env",
)
# Goals that stay OUT of SIDE_EFFECT_FREE, each with the reason. A goal that builds, reads or ships the ARM
# artifacts (or writes under pkg/) must reset the stamp: that is how a stale binary is never deployed.
STAMPED_GOALS = {
    # they build or ship the binary the stamp describes
    "all": "builds pkg/plxnative", "deploy": "builds and ships pkg/plxnative", "install": "builds and ships the ipk",
    "ipk": "packages pkg/plxnative", "test": "is `deploy run`", "symbols": "reads pkg/plxnative",
    "symbols-libs": "reads pkg/ libraries", "sentry-symbols": "reads pkg/plxnative",
    "libass": "builds the staged ARM libass", "sentry-native": "builds the staged ARM sentry-native",
    "clean": "deletes pkg/plxnative on purpose (the control in this file)",
    # host builds, but they stage host libraries into pkg/ or package from it
    "libass-host": "builds pkg/libass-plx.0.dylib", "check-ass": "needs the host libass in pkg/",
    "check-ffmpeg": "needs the host FFmpeg in pkg/", "sim": "needs the host FFmpeg in pkg/",
    "sim-macos": "needs the host FFmpeg in pkg/", "sim-linux": "needs the host libass in pkg/",
    "sim-wsl": "needs the host libass in pkg/", "site-video-sim": "needs the host libass in pkg/",
    "screenshots": "builds screenshots-sim", "screenshots-sim": "needs the host FFmpeg in pkg/",
    "check-replay": "builds sim-macos", "sim-run": "builds sim-macos", "sim-shot": "builds sim-macos",
    "sim-play": "builds sim-macos", "sim-macos-run": "builds sim-macos", "sim-macos-shot": "builds sim-macos",
    "sim-macos-play": "builds sim-macos", "macapp": "packages from and into pkg/", "macapp-zip": "packages from and into pkg/",
    # ARM probes cross-compiled into pkg/
    "threadprobe": "builds pkg/threadprobe", "sockprobe": "builds pkg/sockprobe", "logmprobe": "builds pkg/logmprobe",
    "mali-hwcnt-probe": "builds pkg/mali-hwcnt-probe", "tv-capture-bench": "builds pkg/tv-capture-bench",
    "mali-irq-sample": "builds pkg/mali-irq-sample", "plxnative-stackwalk": "builds pkg/plxnative-stackwalk",
    # a build INPUT that lives under pkg/: writing it changes TELEMETRY_CFG, which is in the stamp
    "telemetry-local": "writes pkg/telemetry.local.json, an input of the stamp",
    # `check` and the goals that re-enter it are a mix: a step of check-python-rest (ci/test_disk_preflight.py,
    # `test_make_n_runs_the_preflight_before_the_build`) runs `make -n` of the ARM library and of
    # pkg/plxnative-storage at the checkout's root, and check-cargo-unit-default grades pkg/plxnative-storage
    # when it exists (ci/test_storage_service_package.py). Listing them would only move the reset into a
    # nested make.
    "check": "mix: its steps run make -n of ARM goals in the checkout",
    "check-unlocked": "mix: the body of check", "check-python": "mix: runs check-python-rest",
    "check-python-rest": "mix: its steps run make -n of ARM goals in the checkout",
    "check-cargo": "mix: runs check-cargo-unit-default",
    "check-cargo-unit-default": "mix: grades pkg/plxnative-storage when it exists",
}
# What the Makefile's parse-time reset `rm -f`s when the configuration changed, plus the stamp itself.
RESET_PATHS = ("pkg/plxnative", "pkg/plxnative-storage", "pkg/libavformat-plx.so.58", "pkg/.ffabi-ok",
               "vendor/ffmpeg-prefix/include/libavformat/avformat.h")
STAMP = "pkg/.build-config"
PLACEHOLDER_TV = "198.51.100.7"  # TEST-NET-2: a documentation address, never a television


class ScratchTree(unittest.TestCase):
    """A scratch checkout to run `make -n` in, seeded the way `make ARM_PROFILE=release` leaves a tree.

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

    def assert_keeps_the_build(self, goal: str):
        seeded = self.seed_release_build()
        deleted, stamp_changed, stamp_is_tvdev = self.run_goal(goal, seeded)
        self.assertEqual(deleted, [], f"make {goal} deleted a release build's files")
        self.assertFalse(stamp_changed, f"make {goal} rewrote the release stamp"
                                        f"{' to the tvdev configuration' if stamp_is_tvdev else ''}")


class DeviceControlGoalsKeepTheBuild(ScratchTree):
    """`tools/tv-session.sh` closes the app with a plain `make kill` right after a
    `make ARM_PROFILE=release` build. `kill` builds nothing, but it was not in `SIDE_EFFECT_FREE`, so
    its (tvdev) configuration differed from the release stamp and the Makefile deleted `pkg/plxnative`
    and rewrote the stamp: the next `up` died on "no pkg/plxnative -- run make".
    """

    def test_goals_that_only_drive_the_tv_keep_a_release_build(self):
        for goal in ("print-appid",) + DEVICE_CONTROL_GOALS:  # print-appid: an exempt goal that must pass today
            with self.subTest(goal):
                self.assert_keeps_the_build(goal)

    def test_a_goal_that_builds_still_resets_the_stamp(self):
        """The control: without it a parse that never reached the reset would pass the test above."""
        seeded = self.seed_release_build()
        deleted, stamp_changed, stamp_is_tvdev = self.run_goal("clean", seeded)
        self.assertEqual(deleted, list(RESET_PATHS))
        self.assertTrue(stamp_changed and stamp_is_tvdev)


class HostOnlyGoalsKeepTheBuild(ScratchTree):
    """`make test-crate C=plx_gfx` after a `make ARM_PROFILE=release` build deleted `pkg/plxnative` and
    rewrote the stamp to `features:+profile:tvdev+tel:...` (2026-10-24). It builds and runs one crate's lib
    tests in `rust-modules/target-fast` and never produces or reads `pkg/plxnative`; `test-hostsim` and
    `check-nodef` were exempt and it had been missed. The rule is "does the goal build, read or ship an
    ARM artifact or anything under pkg/", so this pins the whole set of goals that do not, and (below)
    the ones that do.
    """

    def test_host_only_goals_keep_a_release_build(self):
        for goal in HOST_ONLY_GOALS:
            with self.subTest(goal):
                self.assert_keeps_the_build(goal)

    def test_goals_that_build_or_ship_stay_out_of_the_list(self):
        listed = re.search(r"^SIDE_EFFECT_FREE = (.*)$", (ROOT / "Makefile").read_text(), flags=re.M)
        self.assertIsNotNone(listed, "the Makefile's SIDE_EFFECT_FREE must stay ONE line "
                                     "(`grep '^SIDE_EFFECT_FREE' Makefile` is how the docs say to read it)")
        free = set(listed.group(1).split())
        for goal, why in STAMPED_GOALS.items():
            with self.subTest(goal):
                self.assertNotIn(goal, free, f"{goal} {why}: it must reset the stamp, not be exempt from it")

    def test_the_lists_do_not_overlap(self):
        self.assertEqual(set(HOST_ONLY_GOALS) & set(STAMPED_GOALS), set())
        self.assertEqual(set(HOST_ONLY_GOALS) & set(DEVICE_CONTROL_GOALS), set())
        self.assertEqual(set(DEVICE_CONTROL_GOALS) & set(STAMPED_GOALS), set())


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
