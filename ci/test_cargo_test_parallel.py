#!/usr/bin/env python3
"""tools/cargo-test-parallel.py keeps `make check` honest while running the test binaries side by side.

A fake `cargo` (a script that reports one fake test executable per `-p`) and fake executables
stand in for the real thing; nothing is compiled. What is pinned is what would silently weaken
the gate:

* a failing test binary fails the run (exit 101) and the OTHER binaries still run and print;
* every binary's literal `test result:` line reaches stdout;
* a package that built no test executable, or a build that reported none, is a failure, not a pass;
* a failed build returns cargo's own status and runs nothing;
* each binary gets its own PLXNATIVE_RUNTIME_DIR under the caller's, runs in its package directory,
  and receives the --filter (and only there);
* a binary killed by a signal fails the run.
"""
from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RUNNER = ROOT / "tools" / "cargo-test-parallel.py"

FAKE_CARGO = """#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
home = os.environ["FAKE_HOME"]
if os.environ.get("FAKE_BUILD_EXIT"):
    print("error: could not compile `plx_base`", file=sys.stderr)
    sys.exit(int(os.environ["FAKE_BUILD_EXIT"]))
skip = set(os.environ.get("FAKE_NO_EXECUTABLE", "").split())
print("   Compiling something v0.0.0", file=sys.stderr)
for i, a in enumerate(args):
    if a != "-p" or args[i + 1] in skip:
        continue
    name = args[i + 1]
    pkg = os.path.join(home, "pkgs", name)
    os.makedirs(pkg, exist_ok=True)
    print(json.dumps({"reason": "compiler-artifact", "package_id": "path+file:///r/%s#%s@0.0.0" % (name, name),
                      "manifest_path": os.path.join(pkg, "Cargo.toml"), "target": {"name": name},
                      "profile": {"test": True}, "executable": os.path.join(home, "bins", name)}))
"""

# Behaviour by package name: `fail_*` exits 101 after printing a failing result, `kill_*` SIGKILLs
# itself, anything else passes. It records cwd/runtime/args so the tests can read them back.
FAKE_BIN = """#!/bin/sh
name=$(basename "$0")
echo "bin=$name cwd=$(pwd) runtime=$PLXNATIVE_RUNTIME_DIR args=$*" >> "$FAKE_HOME/bins.log"
case "$name" in
  fail_*) echo "test result: FAILED. 0 passed; 1 failed; 0 ignored"; exit 101;;
  kill_*) kill -9 $$;;
esac
echo "running 1 test"
echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"
"""


class Fixture:
    def __enter__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(self.tmp.name)
        (self.home / "bins").mkdir()
        cargo = self.home / "cargo"
        cargo.write_text(FAKE_CARGO)
        cargo.chmod(0o755)
        self.cargo = str(cargo)
        return self

    def __exit__(self, *exc):
        self.tmp.cleanup()

    def binary(self, name):
        path = self.home / "bins" / name
        path.write_text(FAKE_BIN)
        path.chmod(path.stat().st_mode | stat.S_IXUSR)

    def run(self, packages, *runner_args, env=None):
        for name in packages:
            self.binary(name)
        runtime = self.home / "runtime"
        runtime.mkdir(exist_ok=True)
        full = dict(os.environ, FAKE_HOME=str(self.home), PLXNATIVE_RUNTIME_DIR=str(runtime), **(env or {}))
        cmd = [sys.executable, str(RUNNER), *runner_args, "--", self.cargo, "+nightly", "test", "--lib"]
        for name in packages:
            cmd += ["-p", name]
        return subprocess.run(cmd, capture_output=True, text=True, env=full, timeout=60)

    def started(self):
        log = self.home / "bins.log"
        return sorted(log.read_text().splitlines()) if log.exists() else []


class RunnerGate(unittest.TestCase):
    def test_every_binary_runs_and_its_result_line_is_printed(self):
        with Fixture() as f:
            out = f.run(["plx_a", "plx_b", "plx_c"])
            self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
            self.assertEqual(out.stdout.count("test result: ok. 1 passed"), 3)
            self.assertEqual(len(f.started()), 3)
            self.assertIn("3 test binaries", out.stdout)

    def test_a_failing_binary_fails_the_run_but_the_others_still_run(self):
        with Fixture() as f:
            out = f.run(["plx_a", "fail_b", "plx_c"], "--jobs", "1")
            self.assertEqual(out.returncode, 101, out.stdout + out.stderr)
            self.assertEqual(len(f.started()), 3, "a failure must not hide the binaries after it")
            self.assertIn("test result: FAILED. 0 passed; 1 failed", out.stdout)
            self.assertEqual(out.stdout.count("test result: ok. 1 passed"), 2)
            self.assertIn("FAILED fail_b (exit 101)", out.stdout)

    def test_a_binary_killed_by_a_signal_fails_the_run(self):
        with Fixture() as f:
            out = f.run(["plx_a", "kill_b"])
            self.assertNotEqual(out.returncode, 0, out.stdout + out.stderr)
            self.assertIn("FAILED kill_b", out.stdout)

    def test_a_package_without_a_test_executable_is_not_a_pass(self):
        with Fixture() as f:
            out = f.run(["plx_a", "plx_b"], env={"FAKE_NO_EXECUTABLE": "plx_b"})
            self.assertEqual(out.returncode, 1, out.stdout + out.stderr)
            self.assertIn("asked for 2 packages", out.stderr)
            self.assertEqual(f.started(), [], "nothing runs once the gate is known to be short")

    def test_no_executable_at_all_is_not_a_pass(self):
        with Fixture() as f:
            out = f.run(["plx_a"], env={"FAKE_NO_EXECUTABLE": "plx_a"})
            self.assertEqual(out.returncode, 1, out.stdout + out.stderr)
            self.assertIn("no test executable", out.stderr)

    def test_a_failed_build_returns_cargos_status_and_runs_nothing(self):
        with Fixture() as f:
            out = f.run(["plx_a"], env={"FAKE_BUILD_EXIT": "101"})
            self.assertEqual(out.returncode, 101)
            self.assertIn("could not compile", out.stderr)
            self.assertEqual(f.started(), [])
            out = f.run(["plx_a"], env={"FAKE_BUILD_EXIT": "7"})
            self.assertEqual(out.returncode, 7)

    def test_compiler_progress_reaches_the_terminal_untouched(self):
        with Fixture() as f:
            out = f.run(["plx_a"])
            self.assertIn("Compiling something v0.0.0", out.stderr)

    def test_the_command_is_not_allowed_to_set_its_own_no_run(self):
        with Fixture() as f:
            f.binary("plx_a")
            cmd = [sys.executable, str(RUNNER), "--", f.cargo, "test", "--lib", "-p", "plx_a", "--no-run"]
            out = subprocess.run(cmd, capture_output=True, text=True, env=dict(os.environ, FAKE_HOME=str(f.home)))
            self.assertNotEqual(out.returncode, 0)


class RunnerEnvironment(unittest.TestCase):
    def test_each_binary_gets_its_own_runtime_dir_under_the_callers(self):
        with Fixture() as f:
            out = f.run(["plx_a", "plx_b"])
            self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
            runtimes = {}
            for line in f.started():
                fields = dict(part.split("=", 1) for part in line.split(" ") if "=" in part)
                runtimes[fields["bin"]] = fields["runtime"]
            self.assertEqual(set(runtimes), {"plx_a", "plx_b"})
            self.assertEqual(len(set(runtimes.values())), 2, "two binaries must not share a runtime root")
            for name, path in runtimes.items():
                self.assertEqual(path, str(f.home / "runtime" / name))

    def test_binaries_run_in_their_package_directory(self):
        with Fixture() as f:
            f.run(["plx_a"])
            (line,) = f.started()
            cwd = dict(part.split("=", 1) for part in line.split(" ") if "=" in part)["cwd"]
            self.assertEqual(os.path.realpath(cwd), os.path.realpath(f.home / "pkgs" / "plx_a"))

    def test_the_filter_reaches_every_binary_and_only_there(self):
        with Fixture() as f:
            out = f.run(["plx_a", "plx_b"], "--filter", "route::")
            self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
            self.assertTrue(all(line.endswith("args=route::") for line in f.started()), f.started())

    def test_no_runtime_dir_from_the_caller_gets_a_private_one_that_is_removed(self):
        with Fixture() as f:
            f.binary("plx_a")
            env = {k: v for k, v in os.environ.items() if k != "PLXNATIVE_RUNTIME_DIR"}
            env["FAKE_HOME"] = str(f.home)
            cmd = [sys.executable, str(RUNNER), "--", f.cargo, "test", "--lib", "-p", "plx_a"]
            out = subprocess.run(cmd, capture_output=True, text=True, env=env)
            self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
            (line,) = f.started()
            runtime = dict(part.split("=", 1) for part in line.split(" ") if "=" in part)["runtime"]
            self.assertIn("plxnative-check.", runtime)
            self.assertFalse(Path(runtime).exists())


class RealCargoJson(unittest.TestCase):
    """The parser against the shape real cargo prints, captured as a literal."""

    def test_only_test_profile_artifacts_with_an_executable_count(self):
        sys.path.insert(0, str(ROOT / "tools"))
        import importlib.util
        spec = importlib.util.spec_from_file_location("ctp", RUNNER)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        lines = [
            json.dumps({"reason": "compiler-artifact", "package_id": "x#a@0", "executable": None,
                        "profile": {"test": False}, "target": {"name": "a"}}),
            json.dumps({"reason": "compiler-artifact", "package_id": "x#a@0", "executable": "/t/a-1",
                        "profile": {"test": True}, "target": {"name": "a"}, "manifest_path": "/r/a/Cargo.toml"}),
            json.dumps({"reason": "build-script-executed", "package_id": "x#a@0",
                        "linked_paths": ["native=/opt/lib"]}),
            "not json at all",
            json.dumps({"reason": "build-finished", "success": True}),
        ]
        tests, link = mod.collect(lines)
        self.assertEqual([t["executable"] for t in tests], ["/t/a-1"])
        self.assertEqual(link, {"x#a@0": ["native=/opt/lib"]})
        self.assertEqual(mod.requested_packages(["cargo", "test", "-p", "a", "--package", "b", "--package=c"]),
                         ["a", "b", "c"])


if __name__ == "__main__":
    unittest.main()
