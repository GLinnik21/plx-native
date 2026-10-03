#!/usr/bin/env python3
"""`make build-bench` (tools/build-bench.py): the shape of what it prints, and the fences around it.

Hermetic. The benchmark runs against a SCRATCH git repository (`--repo`) whose Makefile answers
`print-bench-config` with canned values, and a FAKE `cargo` / `rustc` / `uptime` / `sysctl` in a
throwaway HOME's `.cargo/bin` (the script puts that first on PATH, as the Makefile's recipes do).
Nothing is compiled and no real source file is edited. What this pins:

* the Markdown table and the `--json` document have the documented shape, every timed scenario
  ran `--runs` times, interleaved, and a no-op build that recompiled the app crate is flagged;
* the edit targets are restored byte for byte even when cargo FAILS mid-edit, and a target that is
  dirty at the start is refused before any cargo runs;
* RELEASE=1 is refused (environment, Makefile config, and `make build-bench` itself);
* an absent target-fast tree or ARM archive is a SKIP with a note, never a build;
* the load / swap noise warning fires and stays quiet;
* the cargo invocations are the Makefile's: the mirrored subcommand skeletons are compared with the
  real recipes, `print-bench-config` carries no telemetry credential, and the Makefile wiring
  (lock wrapper, `--quick`, SIDE_EFFECT_FREE, check-python registration) is present.
"""
from __future__ import annotations

import importlib.util
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "tools" / "build-bench.py"

spec = importlib.util.spec_from_file_location("build_bench", SCRIPT)
bb = importlib.util.module_from_spec(spec)
sys.modules["build_bench"] = bb
spec.loader.exec_module(bb)

FAKE_CARGO = r'''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[2:]  # argv[1] is the +toolchain
log = os.environ["FAKE_LOG"]
with open(log, "a") as f:
    f.write(json.dumps({"args": args, "cwd": os.getcwd(), "incr": os.environ.get("CARGO_INCREMENTAL"),
                        "tdir": os.environ.get("CARGO_TARGET_DIR"), "rustflags": os.environ.get("RUSTFLAGS"),
                        "runtime": bool(os.environ.get("PLXNATIVE_RUNTIME_DIR"))}) + "\n")
def read(p):
    try:
        return open(p).read()
    except OSError:
        return ""
EDITED = ("base/src/cbuf.rs", "src/coldstart.rs", "src/ui/mod.rs")
if os.environ.get("FAKE_FAIL_ON_EDIT") and "build-bench edit" in "".join(read(p) for p in EDITED):
    sys.stderr.write("error: fake cargo failed on the edited tree\n")
    sys.exit(101)
if "--no-run" in args:
    # cargo judges a path package by mtime: a restored file is stale until the next build
    sig = "".join(read(p) for p in EDITED) + str([os.stat(p).st_mtime_ns for p in EDITED])
    state = os.environ["FAKE_STATE"] + "." + (os.environ.get("CARGO_TARGET_DIR") or "target").replace("/", "_")
    fresh = read(state) == sig and not os.environ.get("FAKE_ALWAYS_DIRTY")
    open(state, "w").write(sig)
    print("   Compiling noise on stdout that is not JSON")
    print(json.dumps({"reason": "compiler-artifact", "package_id": "registry+x#serde@1.0.0", "fresh": True}))
    print(json.dumps({"reason": "compiler-artifact", "package_id": "path+file:///r/rust-modules/base#plx_base@0.0.0", "fresh": fresh}))
    print(json.dumps({"reason": "compiler-artifact", "package_id": "path+file:///r/rust-modules#plxnative-modules@0.7.0", "fresh": fresh}))
elif args[:2] == ["test", "--lib"]:
    print("running 5432 tests")
    print("test result: ok. 5300 passed; 0 failed; 7 ignored; 0 measured; 0 filtered out; finished in 1.00s")
    print("test result: ok. 125 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s")
elif args[0] == "rustc":
    tgt = args[args.index("--target") + 1]
    tdir = args[args.index("--target-dir") + 1]
    out = os.path.join(tdir, tgt, "release")
    os.makedirs(out, exist_ok=True)
    open(os.path.join(out, "libplxnative_modules.a"), "wb").write(b"!<arch>\nfake archive")
    print(json.dumps({"reason": "compiler-artifact", "fresh": False}))
'''
FAKE_RUSTC = "#!/bin/sh\necho 'rustc 1.99.0-nightly (fake 2026-01-01)'\n"
FAKE_UPTIME = '#!/bin/sh\necho " 3:30  up 1 day, load averages: ${FAKE_LOAD:-0.10} 0.2 0.3"\n'
FAKE_SYSCTL = ('#!/bin/sh\ncase "$2" in\n'
               '  vm.swapusage) echo "total = 1000.00M  used = ${FAKE_SWAP_USED:-100.00}M  free = 1.00M  (encrypted)";;\n'
               '  *) echo "Fake CPU";;\nesac\n')
SCRATCH_MAKEFILE = r'''print-bench-config:
	@printf '%s\n' 'RUST_NIGHTLY=nightly' 'RUST_TDIR=target' 'RUST_TARGET=arm-unknown-linux-gnueabi' \
	  'RUST_FEATFLAGS=' 'RUST_LIB=rust-modules/target/arm-unknown-linux-gnueabi/release/libplxnative_modules.a' \
	  'RUST_ENV=RUSTFLAGS="-C target-cpu=fake -C x"' 'TEST_FAST_TDIR=target-fast' 'RELEASE=$(RELEASE)'
'''
LEAF = "rust-modules/base/src/cbuf.rs"
APP_LEAF = "rust-modules/src/coldstart.rs"
HUB = "rust-modules/src/ui/mod.rs"
SRC_FILES = {LEAF: "pub fn leaf() {}\n", APP_LEAF: "pub fn app() {}\n", HUB: "pub fn hub() {}",
             "rust-modules/src/lib.rs": "mod coldstart;\n"}


def git(repo, *args):
    return subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", *args],
                          cwd=repo, capture_output=True, text=True, check=True)


class Sandbox:
    """A scratch repo, a fake toolchain home, and a way to run the script against them."""

    def __enter__(self):
        self._tmp = tempfile.TemporaryDirectory()
        base = Path(self._tmp.name)
        self.repo = base / "repo"
        self.home = base / "home"
        bindir = self.home / ".cargo" / "bin"
        bindir.mkdir(parents=True)
        for name, body in (("cargo", FAKE_CARGO), ("rustc", FAKE_RUSTC), ("uptime", FAKE_UPTIME),
                           ("sysctl", FAKE_SYSCTL)):
            (bindir / name).write_text(body)
            (bindir / name).chmod(0o755)
        for rel, body in SRC_FILES.items():
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / rel).write_text(body)
        (self.repo / "Makefile").write_text(SCRATCH_MAKEFILE)
        git(self.repo, "init", "-q")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", "scratch")
        self.log = base / "cargo.log"
        self.state = base / "fresh-state"
        return self

    def __exit__(self, *exc):
        self._tmp.cleanup()

    def run(self, *args, env=None):
        full = {k: v for k, v in os.environ.items() if k not in ("RELEASE", "CARGO_TARGET_DIR", "MAKEFLAGS")}
        full.update(HOME=str(self.home), FAKE_LOG=str(self.log), FAKE_STATE=str(self.state))
        full.update(env or {})
        return subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo), *args],
                              capture_output=True, text=True, env=full)

    def calls(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def assert_sources_untouched(self, test):
        for rel, body in SRC_FILES.items():
            test.assertEqual((self.repo / rel).read_text(), body, f"{rel} was not restored")
        test.assertEqual(git(self.repo, "status", "--porcelain", "--untracked-files=no").stdout.strip(), "")


class ShapeTests(unittest.TestCase):
    def test_table_json_and_restoration(self):
        with Sandbox() as sb:
            (sb.repo / "rust-modules" / "target").mkdir(parents=True)
            (sb.repo / "rust-modules" / "target" / "blob").write_bytes(b"x" * 4096)
            out = sb.repo.parent / "out.json"
            proc = sb.run("--runs", "2", "--only", "noop,leaf,hub,tests,sizes", "--json", str(out))
            self.assertEqual(proc.returncode, 0, proc.stderr)
            md = proc.stdout
            self.assertIn("| Scenario | Runs | Median | Min | Max | Notes |", md)
            for title in (bb.TITLES["noop"], bb.TITLES["leaf"], bb.TITLES["hub"], bb.TITLES["tests"]):
                self.assertRegex(md, re.escape(f"| {title} | 2 | ") + r"[\d.]+ s \| [\d.]+ s \| [\d.]+ s \|")
            self.assertIn("app crate rebuilt: no", md)   # the no-op row
            self.assertIn("app crate rebuilt: yes", md)  # the edit rows
            self.assertIn("5425 passed / 0 failed / 7 ignored", md)
            self.assertIn("| Target dirs (1) |", md)
            self.assertNotIn("WARNING", md)

            doc = json.loads(out.read_text())
            self.assertEqual(doc["schema"], 1)
            self.assertRegex(doc["git_sha"], r"^[0-9a-f]{12}$")
            self.assertIn("fake", doc["toolchain"])
            self.assertTrue(doc["host"]["machine"] and doc["host"]["os"])
            self.assertGreaterEqual(doc["host"]["cores"], 1)
            self.assertEqual(doc["runs"], 2)
            self.assertTrue(doc["restored_clean"])
            self.assertEqual([s["id"] for s in doc["scenarios"]], ["noop", "leaf", "hub", "tests", "sizes"])
            for sc in doc["scenarios"]:
                if sc["id"] == "sizes":
                    continue
                self.assertEqual(len(sc["samples"]), 2)
                self.assertLessEqual(sc["min"], sc["median"])
                self.assertLessEqual(sc["median"], sc["max"])
                for sample in sc["samples"]:
                    self.assertIn("load1", sample)
                    self.assertIn("swap_used_pct", sample)
            by_id = {s["id"]: s for s in doc["scenarios"]}
            self.assertFalse(any(s["app_rebuilt"] for s in by_id["noop"]["samples"]))
            self.assertTrue(all(s["app_rebuilt"] for s in by_id["leaf"]["samples"]))
            self.assertEqual(by_id["tests"]["samples"][0]["passed"], 5425)
            self.assertEqual(doc["sizes"]["count"], 1)
            self.assertNotIn("PLX_", out.read_text())
            sb.assert_sources_untouched(self)

    def test_rounds_interleave_the_scenarios(self):
        with Sandbox() as sb:
            proc = sb.run("--runs", "2", "--only", "noop,leaf,hub")
            self.assertEqual(proc.returncode, 0, proc.stderr)
            rounds = re.findall(r"round (\d)/2: (\w+)", proc.stderr)
            self.assertEqual(rounds, [("1", "noop"), ("1", "leaf"), ("1", "hub"),
                                      ("2", "noop"), ("2", "leaf"), ("2", "hub")])

    def test_quick_is_noop_leaf_and_sizes_at_one_run(self):
        with Sandbox() as sb:
            out = sb.repo.parent / "q.json"
            proc = sb.run("--quick", "--json", str(out))
            self.assertEqual(proc.returncode, 0, proc.stderr)
            doc = json.loads(out.read_text())
            self.assertEqual(doc["runs"], 1)
            self.assertEqual([s["id"] for s in doc["scenarios"]], ["noop", "leaf", "sizes"])

    def test_cargo_is_called_the_way_the_makefile_does(self):
        with Sandbox() as sb:
            (sb.repo / "rust-modules" / "target-fast").mkdir(parents=True)
            lib = sb.repo / "rust-modules/target/arm-unknown-linux-gnueabi/release/libplxnative_modules.a"
            lib.parent.mkdir(parents=True)
            lib.write_bytes(b"old")
            proc = sb.run("--runs", "1", "--only", "noop,leaf-inc,tests,arm")
            self.assertEqual(proc.returncode, 0, proc.stderr)
            calls = sb.calls()
            for c in calls:
                self.assertTrue(c["cwd"].endswith("rust-modules"), c)
            plain = [c for c in calls if c["args"][:2] == ["test", "--lib"] and c["incr"] == "0"]
            self.assertTrue(plain)
            self.assertTrue(all(c["tdir"] is None and c["runtime"] for c in plain), plain)
            inc = [c for c in calls if c["incr"] == "1"]
            self.assertTrue(inc)
            self.assertTrue(all(c["tdir"] == "target-fast" and c["args"] == list(bb.HOST_TEST_BUILD_ARGS)
                                for c in inc), inc)
            self.assertIn(list(bb.HOST_TEST_ARGS), [c["args"] for c in plain])  # the suite itself
            arm = [c for c in calls if c["args"][0] == "rustc"]
            self.assertEqual(len(arm), 1)
            self.assertEqual(arm[0]["args"], ["rustc", "--release", "--target", "arm-unknown-linux-gnueabi",
                                              "--lib", "--crate-type", "staticlib", "--target-dir", "target",
                                              "--message-format=json-render-diagnostics"])
            self.assertEqual(arm[0]["incr"], "0")
            self.assertEqual(arm[0]["rustflags"], "-C target-cpu=fake -C x")
            self.assertIn("archive 0.0 MiB, sha256 ", proc.stdout)


class FenceTests(unittest.TestCase):
    def test_edit_targets_are_restored_when_cargo_fails(self):
        with Sandbox() as sb:
            proc = sb.run("--runs", "1", "--only", "noop,leaf,hub", env={"FAKE_FAIL_ON_EDIT": "1"})
            self.assertEqual(proc.returncode, 1, proc.stderr)
            self.assertIn("FAILED:", proc.stdout)
            self.assertIn("fake cargo failed on the edited tree", proc.stdout)
            self.assertRegex(proc.stdout, r"\| " + re.escape(bb.TITLES["leaf"]) + r" \| 0 \| failed")
            sb.assert_sources_untouched(self)

    def test_a_dirty_edit_target_is_refused_before_any_cargo(self):
        with Sandbox() as sb:
            (sb.repo / LEAF).write_text("pub fn leaf() {} // someone's work\n")
            proc = sb.run("--only", "leaf")
            self.assertEqual(proc.returncode, 2)
            self.assertIn("refusing to start", proc.stderr)
            self.assertIn(LEAF, proc.stderr)
            self.assertEqual(sb.calls(), [])
            self.assertEqual((sb.repo / LEAF).read_text(), "pub fn leaf() {} // someone's work\n")

    def test_a_dirty_file_that_is_not_edited_does_not_matter(self):
        with Sandbox() as sb:
            (sb.repo / HUB).write_text("changed")
            proc = sb.run("--runs", "1", "--only", "noop,leaf")
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertEqual((sb.repo / HUB).read_text(), "changed")

    def test_release_is_refused_from_the_environment(self):
        with Sandbox() as sb:
            proc = sb.run("--only", "noop", env={"RELEASE": "1"})
            self.assertEqual(proc.returncode, 2)
            self.assertIn("RELEASE=1", proc.stderr)
            self.assertEqual(sb.calls(), [])

    def test_release_is_refused_from_the_makefile_config(self):
        with Sandbox() as sb:
            (sb.repo / "Makefile").write_text(SCRATCH_MAKEFILE.replace("'RELEASE=$(RELEASE)'", "'RELEASE=1'"))
            proc = sb.run("--only", "noop")
            self.assertEqual(proc.returncode, 2)
            self.assertIn("RELEASE=1", proc.stderr)
            self.assertEqual(sb.calls(), [])

    def test_unknown_scenario_is_refused(self):
        with Sandbox() as sb:
            self.assertEqual(sb.run("--only", "nope").returncode, 2)


class SkipTests(unittest.TestCase):
    def test_absent_incremental_tree_and_arm_archive_are_skipped_with_a_note(self):
        with Sandbox() as sb:
            out = sb.repo.parent / "s.json"
            proc = sb.run("--runs", "1", "--only", "noop,leaf-inc,hub-inc,arm", "--json", str(out))
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertRegex(proc.stdout, r"\| " + re.escape(bb.TITLES["leaf-inc"]) + r" \| - \| skipped .*target-fast.* is absent")
            self.assertRegex(proc.stdout, r"\| " + re.escape(bb.TITLES["arm"]) + r" \| - \| skipped .*no ARM staticlib")
            self.assertIn("never triggers the FFmpeg build", proc.stdout)
            doc = json.loads(out.read_text())
            by_id = {s["id"]: s for s in doc["scenarios"]}
            for sid in ("leaf-inc", "hub-inc", "arm"):
                self.assertEqual(by_id[sid]["status"], "skipped")
                self.assertTrue(by_id[sid]["note"])
            self.assertEqual([c for c in sb.calls() if c["args"][0] == "rustc"], [])
            self.assertEqual([c for c in sb.calls() if c["incr"] == "1"], [])
            self.assertFalse((sb.repo / "rust-modules" / "target-fast").exists())

    def test_an_edit_in_one_tree_does_not_make_the_next_noop_look_stale(self):
        # The restored file carries a new mtime, which stales BOTH target dirs, so every row that
        # needs a built tree rebuilds it first, untimed. Without that the no-op row timed a rebuild.
        with Sandbox() as sb:
            out = sb.repo.parent / "m.json"
            proc = sb.run("--runs", "3", "--only", "noop,leaf-inc", "--cold", "--json", str(out))
            self.assertEqual(proc.returncode, 0, proc.stderr)
            noop = next(s for s in json.loads(out.read_text())["scenarios"] if s["id"] == "noop")
            self.assertEqual([s["app_rebuilt"] for s in noop["samples"]], [False, False, False])
            self.assertNotIn("UNEXPECTED", proc.stdout)

    def test_cold_builds_the_incremental_tree_instead_of_skipping(self):
        with Sandbox() as sb:
            proc = sb.run("--runs", "1", "--only", "leaf-inc", "--cold")
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertNotIn("skipped", proc.stdout)
            self.assertTrue([c for c in sb.calls() if c["incr"] == "1"])
            sb.assert_sources_untouched(self)


class NoiseTests(unittest.TestCase):
    def test_load_above_core_count_warns(self):
        with Sandbox() as sb:
            out = sb.repo.parent / "w.json"
            proc = sb.run("--runs", "1", "--only", "noop", "--json", str(out), env={"FAKE_LOAD": "9999.00"})
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIn("WARNING: noop run 1: load 9999.00 >", proc.stdout)
            self.assertIn("numbers are noisy", proc.stdout)
            self.assertTrue(json.loads(out.read_text())["warnings"])

    def test_swap_over_ninety_percent_warns(self):
        with Sandbox() as sb:
            proc = sb.run("--runs", "1", "--only", "noop", env={"FAKE_SWAP_USED": "950.00"})
            self.assertIn("WARNING: noop run 1: swap 95% used", proc.stdout)

    def test_swap_at_ninety_percent_does_not(self):
        with Sandbox() as sb:
            proc = sb.run("--runs", "1", "--only", "noop", env={"FAKE_SWAP_USED": "900.00"})
            self.assertNotIn("WARNING", proc.stdout)

    def test_a_noop_build_that_recompiles_the_app_is_flagged(self):
        with Sandbox() as sb:
            proc = sb.run("--runs", "1", "--only", "noop", env={"FAKE_ALWAYS_DIRTY": "1"})
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIn("UNEXPECTED: a no-op build recompiled the app crate", proc.stdout)

    def test_parsers(self):
        self.assertEqual(bb.parse_load("up 23 days, load averages: 7.62 6.14 6.39"), 7.62)
        self.assertEqual(bb.parse_load("up 1 day, load average: 0.52, 0.40, 0.30"), 0.52)
        self.assertEqual(bb.parse_swap_pct("total = 4096.00M  used = 3048.88M  free = 1047.12M  (encrypted)"), 74.4)
        self.assertEqual(bb.parse_swap_pct("total = 2.00G  used = 1024.00M  free = 1024.00M"), 50.0)
        self.assertIsNone(bb.parse_swap_pct("no swap here"))
        self.assertEqual(bb.parse_test_result("test result: ok. 5425 passed; 0 failed; 7 ignored; 0 measured"),
                         {"status": "ok", "passed": 5425, "failed": 0, "ignored": 7})


class MakefileWiringTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.makefile = (ROOT / "Makefile").read_text()

    def make(self, *args, env=None):
        full = {k: v for k, v in os.environ.items() if k not in ("MAKEFLAGS", "MFLAGS", "RELEASE")}
        full.update(env or {})
        return subprocess.run(["make", *args], cwd=ROOT, capture_output=True, text=True, env=full)

    def recipe(self, header):
        m = re.search(r"^" + re.escape(header) + r".*\n((?:\t.*\n|[ \t]*\n|#.*\n)*)", self.makefile, re.M)
        self.assertIsNotNone(m, header)
        return m.group(1).replace("\\\n", " ")

    def test_host_test_skeleton_matches_the_recipes(self):
        want = " ".join(bb.HOST_TEST_ARGS)
        for header in ("check-cargo-unit-default:", "test-fast:"):
            self.assertRegex(self.recipe(header), r"cargo \+\$\(RUST_NIGHTLY\) " + re.escape(want) + r"(?=\s|$)", header)

    def test_arm_skeleton_matches_the_staticlib_recipe(self):
        recipe = self.recipe("$(RUST_LIB):")
        m = re.search(r"cargo \+\$\(RUST_NIGHTLY\) (rustc .*?--message-format=json-render-diagnostics)", recipe)
        self.assertIsNotNone(m, "the Makefile no longer spells the staticlib cargo line this way")
        want = bb.arm_argv({"RUST_TARGET": "$(RUST_TARGET)", "RUST_TDIR": "$(RUST_TDIR)",
                            "RUST_FEATFLAGS": "$(RUST_FEATFLAGS)"})
        self.assertEqual(shlex.split(m.group(1)), want)

    def test_print_bench_config_reports_the_recipes_values_and_no_credential(self):
        proc = self.make("-s", "print-bench-config", env={"PLX_SENTRY_DSN": "https://secret-dsn.invalid/1",
                                                          "PLX_POSTHOG_KEY": "phc_secretvalue"})
        self.assertEqual(proc.returncode, 0, proc.stderr)
        cfg = dict(line.split("=", 1) for line in proc.stdout.splitlines())
        for key in ("RUST_NIGHTLY", "RUST_TDIR", "RUST_TARGET", "RUST_FEATFLAGS", "RUST_LIB", "RUST_ENV",
                    "TEST_FAST_TDIR", "RELEASE"):
            self.assertIn(key, cfg)
        self.assertEqual(cfg["TEST_FAST_TDIR"], "target-fast")
        self.assertEqual(cfg["RELEASE"], "")
        self.assertTrue(cfg["RUST_ENV"].startswith("RUSTFLAGS="))
        self.assertNotIn("secret", proc.stdout)
        self.assertNotIn("PLX_", proc.stdout)

    def test_make_refuses_release_without_starting_anything(self):
        for goal in ("build-bench", "build-bench-quick"):
            proc = self.make(goal, "RELEASE=1")
            self.assertNotEqual(proc.returncode, 0, goal)
            self.assertIn("refused under RELEASE=1", proc.stderr)
            self.assertNotIn("round 1", proc.stderr)

    def test_dry_run_goes_through_the_lock_with_the_make_environment(self):
        proc = self.make("-n", "build-bench-quick", "ARGS=--runs 2")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertRegex(proc.stdout, r"tools/check-lock\.py -- env PLX_BENCH_VIA_MAKE=1[\s\S]*tools/build-bench\.py --quick --runs 2")
        full = self.make("-n", "build-bench").stdout
        self.assertNotIn("--quick", full)

    def test_the_goals_are_side_effect_free_and_checked(self):
        m = re.search(r"^SIDE_EFFECT_FREE = (.*)$", self.makefile, re.M)
        self.assertIsNotNone(m)
        self.assertTrue({"build-bench", "build-bench-quick"} <= set(m.group(1).split()))
        self.assertIn("python3 ci/test_build_bench.py", self.recipe("check-python:"))


if __name__ == "__main__":
    unittest.main()
