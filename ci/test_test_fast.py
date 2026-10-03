#!/usr/bin/env python3
"""`make test-fast` is an OPT-IN incremental loop, and nothing else may inherit it.

Lanes are non-incremental by default because `debug/incremental` is the largest thing on a lane's
disk (see the Makefile's CARGO_INCREMENTAL comment). `test-fast` spends that disk on purpose, in a
tree of its own, so what this pins is the fence around it:

* `check`, `check-cargo*` and `lint` never mention `CARGO_INCREMENTAL=1` or `target-fast`;
* `test-fast` runs `cargo test --lib` with CARGO_INCREMENTAL=1 and its own CARGO_TARGET_DIR (even
  when the caller's environment says otherwise), inside `rust-modules`, and forwards `T=`;
* `RELEASE=1` refuses before cargo is ever started;
* the directory is gitignored, is not the `target`/`target-release` pair, is invisible to
  `tools/cargo-seed.py` (which handles only those two) and is swept by `tools/build-gc.sh`.

Hermetic: a fake `cargo` in a throwaway HOME (the recipe puts `$HOME/.cargo/bin` first on PATH)
records what it was called with. Nothing is compiled. The ignore check reads .gitignore with
`git check-ignore`, which needs no work tree state.
"""
from __future__ import annotations

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TDIR = "target-fast"

FAKE_CARGO = """#!/bin/sh
{
  echo "cwd=$(pwd)"
  echo "incremental=$CARGO_INCREMENTAL"
  echo "target_dir=$CARGO_TARGET_DIR"
  echo "runtime_dir_set=$([ -n "$PLXNATIVE_RUNTIME_DIR" ] && echo yes || echo no)"
  echo "args=$*"
  echo "--"
} >> "$FAKE_CARGO_LOG"
echo "test result: ok. 1 passed; 0 failed; 0 ignored"
"""


def make(*args, env=None, home=None):
    full = dict(os.environ)
    full.pop("MAKEFLAGS", None)
    full.pop("MFLAGS", None)
    full.update(env or {})
    if home is not None:
        full["HOME"] = home
    return subprocess.run(["make", *args], cwd=ROOT, env=full, capture_output=True, text=True)


class FakeCargoHome:
    def __enter__(self):
        self._tmp = tempfile.TemporaryDirectory()
        home = Path(self._tmp.name)
        bindir = home / ".cargo" / "bin"
        bindir.mkdir(parents=True)
        cargo = bindir / "cargo"
        cargo.write_text(FAKE_CARGO)
        cargo.chmod(0o755)
        self.home = str(home)
        self.log = home / "cargo.log"
        return self

    def __exit__(self, *exc):
        self._tmp.cleanup()

    def run(self, *args, env=None):
        merged = {"FAKE_CARGO_LOG": str(self.log), **(env or {})}
        return make(*args, env=merged, home=self.home)

    def calls(self):
        if not self.log.exists():
            return []
        blocks = [b for b in self.log.read_text().split("--\n") if b.strip()]
        return [dict(line.split("=", 1) for line in b.splitlines()) for b in blocks]


def recipe_text(target):
    """A target's recipe lines, straight from the Makefile (no make run)."""
    lines = (ROOT / "Makefile").read_text().splitlines()
    start = next(i for i, line in enumerate(lines) if line.startswith(target + ":"))
    out = [lines[start]]
    for line in lines[start + 1:]:
        if line and not line.startswith(("\t", "#")):
            break
        out.append(line)
    return "\n".join(out)


class OtherTargetsAreUnaffected(unittest.TestCase):
    def test_check_and_cargo_recipes_never_see_the_fast_loop(self):
        # `make -n` on these only: `check` and `check-unlocked` re-enter make through the machine-wide
        # check lock (a $(MAKE) line runs even under -n), so a dry run of them from INSIDE
        # `make check` waits forever on the lock its own parent holds. They are read as text instead.
        for target in ("check-cargo", "check-cargo-lint", "check-cargo-unit-default",
                       "check-cargo-unit-hostsim", "lint"):
            out = make("-n", target, env={"PLX_CHECK_LOCK": "off"})
            self.assertEqual(out.returncode, 0, f"{target}: {out.stderr}")
            self.assertNotRegex(out.stdout, r"CARGO_INCREMENTAL=1", target)
            self.assertNotIn(TDIR, out.stdout, target)
            self.assertNotIn("test-fast", out.stdout, target)
        for target in ("check", "check-unlocked"):
            text = recipe_text(target)
            self.assertNotRegex(text, r"CARGO_INCREMENTAL=1", target)
            self.assertNotIn(TDIR, text, target)
            self.assertNotIn("test-fast", text, target)

    def test_the_unit_gate_keeps_its_non_incremental_default_shape(self):
        out = make("-n", "check-cargo-lint").stdout
        self.assertIn("CARGO_INCREMENTAL=0", out)


class TestFastRuns(unittest.TestCase):
    def test_incremental_in_its_own_target_dir_and_forwards_the_filter(self):
        with FakeCargoHome() as fake:
            # The caller's environment must not win: a fleet lane exports both of these.
            out = fake.run("test-fast", "T=route::",
                           env={"CARGO_INCREMENTAL": "0", "CARGO_TARGET_DIR": "/somewhere/else"})
            self.assertEqual(out.returncode, 0, out.stderr)
            self.assertIn("test result:", out.stdout)
            self.assertRegex(out.stdout, r"~2\.7 GB")
            self.assertIn("tools/build-gc.sh --incremental", out.stdout)
            (call,) = fake.calls()
            self.assertEqual(call["incremental"], "1")
            self.assertEqual(call["target_dir"], TDIR)
            self.assertTrue(call["cwd"].endswith("rust-modules"), call["cwd"])
            self.assertEqual(call["runtime_dir_set"], "yes")
            self.assertRegex(call["args"], r"^\+\S+ test --lib -p plxnative-modules -p plx_base -p plx_machine -p plx_platform -p plx_gfx route::$")

    def test_no_filter_runs_the_whole_default_feature_suite(self):
        with FakeCargoHome() as fake:
            self.assertEqual(fake.run("test-fast").returncode, 0)
            (call,) = fake.calls()
            self.assertRegex(call["args"], r"^\+\S+ test --lib -p plxnative-modules -p plx_base -p plx_machine -p plx_platform -p plx_gfx$")
            self.assertNotIn("--features", call["args"])

    def test_own_dir_is_neither_of_the_dirs_other_builds_use(self):
        text = (ROOT / "Makefile").read_text()
        self.assertEqual(re.search(r"^TEST_FAST_TDIR\s*=\s*(\S+)", text, flags=re.M).group(1), TDIR)
        self.assertNotIn(TDIR, ("target", "target-release"))

    def test_release_is_refused_before_cargo_starts(self):
        with FakeCargoHome() as fake:
            out = fake.run("test-fast", "RELEASE=1")
            self.assertNotEqual(out.returncode, 0)
            self.assertIn("RELEASE=1", out.stderr)
            self.assertIn("refused", out.stderr)
            self.assertEqual(fake.calls(), [])


class TheDirIsFencedOff(unittest.TestCase):
    def test_gitignored(self):
        out = subprocess.run(["git", "check-ignore", "-q", f"rust-modules/{TDIR}/debug/x"],
                             cwd=ROOT)
        self.assertEqual(out.returncode, 0, f"rust-modules/{TDIR}/ must be in .gitignore")

    def test_cargo_seed_handles_only_target_and_target_release(self):
        text = (ROOT / "tools/cargo-seed.py").read_text()
        kinds = re.search(r"^KINDS\s*=\s*\(([^)]*)\)", text, flags=re.M).group(1)
        self.assertEqual(re.findall(r'"([^"]+)"', kinds), ["target", "target-release"])

    def test_build_gc_incremental_and_lanes_sweep_every_target_star(self):
        text = (ROOT / "tools/build-gc.sh").read_text()
        # Both helpers glob rather than enumerate, so a new `target-*` dir needs no edit there.
        self.assertIn('"$1"/rust-modules/target*/debug/incremental', text)
        self.assertIn('for d in "$1"/rust-modules/target*; do', text)


if __name__ == "__main__":
    unittest.main()
