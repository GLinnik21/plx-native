#!/usr/bin/env python3
"""`make test-hostsim` and `make check-nodef` keep their cargo trees where the disk tooling can see them.

AGENTS.md used to hand out two hand recipes, a `CARGO_TARGET_DIR=/tmp/<dir> cargo test --lib -p
plxnative-modules --features hostsim` and a `CARGO_INCREMENTAL=0 cargo check ... --no-default-features`,
and every agent that ran one picked its own `/tmp/<dir>`. Nothing tracked those: on 2026-10-19 there
were 28 PlxNative cargo trees in `/tmp`, 45.6 GiB, none of them visible to `make disk` or
`tools/build-gc.sh`. The two make targets replace the recipes, and this pins the property that
matters, that neither can put a cargo tree outside the checkout:

* `test-hostsim` builds into `rust-modules/target-hostsim` (relative, so it resolves inside the
  checkout), whatever `CARGO_TARGET_DIR` the caller exported; it is incremental in that dir only,
  runs `plxnative-modules` (or the `C=<crate>` named, spelled as for `test-crate`: `plx_media`,
  `media`, `app`, several allowed; the SAME recipe, so the same dir and the same rules) with
  `--features hostsim`, gives the run a throwaway runtime dir that is removed afterwards, and FAILS
  when the `T=` filter matches no test (libtest says `0 passed` and exits 0, which a mistyped name
  reads exactly like a green run);
* `check-nodef` sets no target dir of its own (so cargo's default, `rust-modules/target`, or the
  `CARGO_TARGET_DIR` a fleet lane exports for `make check` too), runs the non-incremental
  `check --lib --no-default-features` and never carries HOST_ENV or the telemetry words (they would
  change the third-party units it shares with `make lint`);
* neither is part of `make check`, and both sit in SIDE_EFFECT_FREE so a dry run or a call from
  another configuration cannot delete a built release binary;
* the new dir is gitignored (`git add -A` cannot commit it), swept by `tools/build-gc.sh`, and the
  hand recipes are not in the agent-facing docs any more.

Hermetic: a fake `cargo` in a throwaway HOME records what it was called with. Nothing is compiled.
"""
from __future__ import annotations

import os
import re
import subprocess
import unittest
from pathlib import Path

from test_test_fast import FakeCargoHome, make

ROOT = Path(__file__).resolve().parent.parent
TDIR = "target-hostsim"
# What a lane exports for its fleet tree: it must not win over test-hostsim's own dir, and
# check-nodef must follow it (the same tree `make check` uses).
LANE_ENV = {"CARGO_INCREMENTAL": "0", "CARGO_TARGET_DIR": "/somewhere/else", "PLX_CARGO_SEED": "off"}
TARGET_DIR_SPEC = re.compile(r"(?:CARGO_TARGET_DIR=|--target-dir[= ]\s*)(\S+)")


def recipe_body(target):
    """The tab-indented lines of a target's recipe, from the Makefile text (no make run). Unlike
    test_test_fast.recipe_text it stops at the recipe's own end, so the comment block that introduces
    the NEXT target is not read as this one's."""
    lines = (ROOT / "Makefile").read_text().splitlines()
    # The rule itself (`target:` alone), not a target-specific assignment such as `target: DISK_TREE = ...`.
    assign = re.compile(re.escape(target) + r":\s*[A-Za-z_]+\s*[:+?]?=")
    start = next(i for i, line in enumerate(lines) if line.startswith(target + ":") and not assign.match(line))
    body = []
    for line in lines[start + 1:]:
        if line.startswith("\t"):
            body.append(line)
        elif line and not line.startswith("#"):
            break
        elif not line.strip():
            break
    return "\n".join(body)


def inside_checkout(call):
    """Where the recorded cargo call's target dir resolves, relative to the checkout, or None."""
    base = Path(call["cwd"])
    tdir = call["target_dir"]
    if tdir in (None, "None", ""):
        resolved = base / "target"          # cargo's own default: <workspace root>/target
    else:
        resolved = base / tdir
    resolved = Path(os.path.realpath(resolved))
    root = Path(os.path.realpath(ROOT / "rust-modules"))
    return resolved.relative_to(root) if resolved == root or root in resolved.parents else None


class TestHostsim(unittest.TestCase):
    def test_incremental_in_its_own_in_checkout_dir_whatever_the_lane_exports(self):
        with FakeCargoHome() as fake:
            out = fake.run("test-hostsim", "T=lifecycle_regression_tests", env=LANE_ENV)
            self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
            (call,) = fake.calls()
            self.assertEqual(call["incremental"], "1")
            self.assertEqual(call["target_dir"], TDIR)
            self.assertTrue(call["cwd"].endswith("rust-modules"), call["cwd"])
            self.assertEqual(inside_checkout(call), Path(TDIR))
            self.assertEqual(call["runtime_dir_set"], "yes")
            self.assertRegex(call["args"], r"^\+\S+ test --lib -p plxnative-modules --features hostsim --no-run "
                                           r"--message-format=json-render-diagnostics$")
            # One package, so one test binary, and the filter goes to it (the runner builds, then runs).
            (line,) = fake.bin_calls()
            self.assertTrue(line.endswith("args=lifecycle_regression_tests"), line)
            self.assertIn("rust-modules/" + TDIR, out.stdout)

    def test_c_names_the_crate_under_test_and_changes_nothing_else(self):
        # plx_media's own hostsim tests (player/engine.rs, pump.rs, route/decision.rs) are not in the
        # application crate's test binary: C= is how they get the managed dir instead of a /tmp one.
        spellings = {
            "": "-p plxnative-modules",
            "C=app": "-p plxnative-modules",
            "C=plxnative-modules": "-p plxnative-modules",
            "C=plx_media": "-p plx_media",
            "C=media": "-p plx_media",
            "C=plx_ui media": "-p plx_ui -p plx_media",
            "C=app plx_media": "-p plxnative-modules -p plx_media",
        }
        for spelling, packages in spellings.items():
            with self.subTest(spelling=spelling):
                with FakeCargoHome() as fake:
                    args = ["T=some_test"] + ([spelling] if spelling else [])
                    out = fake.run("test-hostsim", *args, env=LANE_ENV)
                    self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
                    (call,) = fake.calls()
                    self.assertEqual(call["incremental"], "1")
                    self.assertEqual(call["target_dir"], TDIR)
                    self.assertEqual(inside_checkout(call), Path(TDIR))
                    self.assertRegex(call["args"], r"^\+\S+ test --lib " + re.escape(packages)
                                     + r" --features hostsim --no-run --message-format=json-render-diagnostics$")
                    self.assertIn(packages.replace("-p ", ""), out.stdout.replace("\n", " "))

    def test_c_keeps_the_filter_and_the_zero_tests_failure(self):
        with FakeCargoHome() as fake:
            out = fake.run("test-hostsim", "C=plx_media", "T=no_such_test", env={"FAKE_PASSED": "0"})
            self.assertNotEqual(out.returncode, 0, out.stdout + out.stderr)
            self.assertIn("matched no test", out.stderr)
        with FakeCargoHome() as fake:
            out = fake.run("test-hostsim", "C=plx_media", "T=a_real_one", env={"FAKE_PASSED": "2"})
            self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
            (line,) = fake.bin_calls()
            self.assertTrue(line.endswith("args=a_real_one"), line)

    def test_c_is_one_recipe_not_a_second_one(self):
        text = (ROOT / "Makefile").read_text()
        self.assertEqual(len(re.findall(r"^test-hostsim:[ \t]*$", text, re.M)), 1)
        self.assertEqual(len(re.findall(r"--features hostsim", recipe_body("test-hostsim"))), 1)

    def test_the_throwaway_runtime_dir_is_removed_afterwards(self):
        with FakeCargoHome() as fake:
            self.assertEqual(fake.run("test-hostsim").returncode, 0)
            (line,) = fake.bin_calls()
            runtime = Path(dict(p.split("=", 1) for p in line.split(" ") if "=" in p)["runtime"])
            self.assertIn("plxnative-check.", str(runtime))
            self.assertFalse(runtime.parent.exists(), f"{runtime.parent} outlived the run")

    def test_a_filter_that_matches_no_test_fails_instead_of_passing_with_zero_tests(self):
        with FakeCargoHome() as fake:
            out = fake.run("test-hostsim", "T=no_such_test", env={"FAKE_PASSED": "0"})
            self.assertNotEqual(out.returncode, 0, out.stdout + out.stderr)
            self.assertIn("matched no test", out.stderr)
            self.assertIn("no_such_test", out.stderr)
        with FakeCargoHome() as fake:
            # ...and the same fake binary, reporting a test, is a pass: the failure is the zero.
            out = fake.run("test-hostsim", "T=no_such_test", env={"FAKE_PASSED": "3"})
            self.assertEqual(out.returncode, 0, out.stdout + out.stderr)

    def test_release_is_refused_before_cargo_starts(self):
        with FakeCargoHome() as fake:
            out = fake.run("test-hostsim", "RELEASE=1")
            self.assertNotEqual(out.returncode, 0)
            self.assertIn("refused", out.stderr)
            self.assertEqual(fake.calls(), [])

    def test_the_recipe_keeps_the_telemetry_and_host_threads_words_like_the_other_host_recipes(self):
        text = recipe_body("test-hostsim")
        self.assertIn("$(TELEMETRY_ENV)", text)
        self.assertIn("$(HOST_ENV)", text)


class TestCheckNodef(unittest.TestCase):
    def test_non_incremental_release_feature_check_in_the_default_tree(self):
        with FakeCargoHome() as fake:
            env = {"CARGO_INCREMENTAL": "1", "PLX_CARGO_SEED": "off"}
            out = fake.run("check-nodef", env=env)
            self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
            (call,) = fake.calls()
            self.assertEqual(call["incremental"], "0")
            self.assertTrue(call["cwd"].endswith("rust-modules"), call["cwd"])
            self.assertRegex(call["args"], r"^\+\S+ check --lib --no-default-features$")
            # No target dir of its own: cargo's default, <workspace>/target, is inside the checkout.
            self.assertIn(call["target_dir"], ("None", "", None))
            self.assertEqual(inside_checkout(call), Path("target"))

    def test_it_follows_the_target_dir_a_lane_exports_for_make_check(self):
        with FakeCargoHome() as fake:
            self.assertEqual(fake.run("check-nodef", env=LANE_ENV).returncode, 0)
            (call,) = fake.calls()
            self.assertEqual(call["target_dir"], "/somewhere/else")
            self.assertEqual(call["incremental"], "0")

    def test_it_shares_make_lints_third_party_units_so_it_must_not_change_their_inputs(self):
        text = recipe_body("check-nodef")
        for banned in ("$(HOST_ENV)", "$(TELEMETRY_ENV)", "RUSTFLAGS", "CARGO_TARGET_DIR=", "--target-dir",
                       "--features", "$(RUST_FEATFLAGS)"):
            self.assertNotIn(banned, text)
        # The seed restore is the one `lint` starts with: a fresh lane's first cargo call must not
        # create an unseeded `target` for every later `make check` to inherit.
        self.assertIn("$(CARGO_SEED) restore target rust-modules/target --env-var CARGO_TARGET_DIR", text)


class NeverOutsideTheCheckout(unittest.TestCase):
    def test_dry_runs_name_no_target_dir_outside_it(self):
        for goal in (("test-hostsim", "T=x"), ("check-nodef",)):
            with self.subTest(goal[0]):
                # CARGO_TARGET_DIR scrubbed: this is what the RECIPE puts in, not what a caller adds.
                out = make("-n", *goal, env={"CARGO_TARGET_DIR": ""}).stdout.splitlines()
                specs = [m for line in out for m in TARGET_DIR_SPEC.findall(line)]
                if goal[0] == "check-nodef":
                    self.assertEqual(specs, [], "check-nodef must inherit the tree make check uses")
                else:
                    self.assertEqual(specs, [TDIR])
                for spec in specs:
                    self.assertFalse(spec.startswith(("/", "~", "$")) or ".." in Path(spec).parts, spec)
                # And no cargo command line mentions /tmp: the only /tmp in the recipe is the
                # throwaway runtime dir, which is `mktemp`'d and removed.
                for line in out:
                    if "cargo +" in line:
                        self.assertNotIn("/tmp", line)

    def test_the_goals_are_side_effect_free_so_a_dry_run_cannot_reset_the_stamp(self):
        text = (ROOT / "Makefile").read_text()
        free = re.search(r"^SIDE_EFFECT_FREE = (.*)$", text, flags=re.M).group(1).split()
        for goal in ("test-hostsim", "check-nodef"):
            self.assertIn(goal, free)

    def test_neither_is_part_of_make_check_nor_of_lint(self):
        for target in ("check", "check-unlocked", "check-cargo", "check-cargo-lint", "check-cargo-unit-default",
                       "check-cargo-unit-hostsim", "lint"):
            text = recipe_body(target)
            for name in (TDIR, "test-hostsim", "check-nodef"):
                self.assertNotIn(name, text, f"{target} mentions {name}")


class TheDirIsManaged(unittest.TestCase):
    def test_gitignored(self):
        out = subprocess.run(["git", "check-ignore", "-q", f"rust-modules/{TDIR}/debug/x"], cwd=ROOT)
        self.assertEqual(out.returncode, 0, f"rust-modules/{TDIR}/ must be in .gitignore")

    def test_build_gc_sweeps_it_without_a_name_list(self):
        text = (ROOT / "tools/build-gc.sh").read_text()
        self.assertIn('for d in "$1"/rust-modules/target*; do', text)
        self.assertIn('"$1"/rust-modules/target*/debug/incremental', text)

    def test_cargo_seed_does_not_touch_it(self):
        text = (ROOT / "tools/cargo-seed.py").read_text()
        kinds = re.search(r"^KINDS\s*=\s*\(([^)]*)\)", text, flags=re.M).group(1)
        self.assertNotIn("hostsim", kinds)

    def test_the_docs_spell_the_plx_media_loop_as_c_not_as_make_check_only(self):
        agents = (ROOT / "AGENTS.md").read_text()
        self.assertIn("make test-hostsim\n  [C=<crate>] [T=filter]", agents)
        self.assertIn("C=plx_media", agents)
        self.assertNotIn("pass alone", agents)
        self.assertIn("make test-hostsim [C=<crate>] [T=filter]", (ROOT / "docs" / "agent-reference.md").read_text())

    def test_the_agent_facing_docs_no_longer_hand_out_a_tmp_cargo_tree(self):
        offenders = []
        for path in [ROOT / "AGENTS.md", ROOT / "CLAUDE.md", ROOT / "docs/agent-reference.md",
                     *(ROOT / ".agents").rglob("*.md")]:
            for number, line in enumerate(path.read_text().splitlines(), 1):
                if re.search(r"CARGO_TARGET_DIR=/tmp|CARGO_TARGET_DIR=\$TMPDIR|--target-dir /tmp", line):
                    offenders.append(f"{path.relative_to(ROOT)}:{number}")
        self.assertEqual(offenders, [], "a /tmp cargo tree is what make test-hostsim / check-nodef replace")


if __name__ == "__main__":
    unittest.main()
