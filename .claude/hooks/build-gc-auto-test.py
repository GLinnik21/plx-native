#!/usr/bin/env python3
"""Cases for the SessionEnd disk-reclaim hook. `python3 .claude/hooks/build-gc-auto-test.py`.

WHAT THIS PROVES, AND WHAT IT CANNOT. SessionEnd hooks share a 1.5s budget across every SessionEnd
hook (raised to this hook's own `timeout`, capped at 60s per code.claude.com/docs/en/hooks) and
CANNOT BLOCK — so the one property that matters is that `.claude/hooks/build-gc-auto.sh` returns
almost instantly regardless of how long the reclaim it launches actually takes. This file proves
exactly that, against a STUB `tools/build-gc.sh` that sleeps for longer than any sane hook timeout
(2s) before writing a marker file: the hook must return in well under a second, and the marker must
still appear a few seconds later, proving the child was truly detached rather than the hook having
silently skipped launching it. It does not prove the REAL script's own `--auto` behaviour — that is
`ci/test_build_gc.py`'s job — only that this hook launches whatever script it resolves and gets out
of the way.

WHY A STUB AND NOT THE REAL SCRIPT. The real `tools/build-gc.sh --auto` can itself run for tens of
seconds (a `--worktrees` pass walks `du` over a whole fleet) and is exactly the kind of build-tool
subprocess this project's test tier rules keep out of a fast host suite — see `ci/test_build_gc.py`'s
own fixtures, which never touch a real disk either. A stub that only sleeps and marks its own
invocation is enough to prove the CONTRACT (fast return, real detach, correct script resolution)
without paying that cost or needing a disposable git repository at all.
"""
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
HOOK = os.path.join(HERE, "build-gc-auto.sh")

STUB = """#!/bin/sh
# Stand-in for tools/build-gc.sh: sleeps past any sane hook timeout, then leaves a marker proving
# it was reached, with what it was invoked with.
sleep 2
echo "stub ran: $*" > "$(dirname "$0")/../ran"
"""


class BuildGcAutoHookTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="build-gc-auto-hook-tests-")
        self.addCleanup(temp.cleanup)
        self.root = temp.name
        # A real (if empty) repo, so `git rev-parse --git-common-dir` — the hook's own way of
        # finding the MAIN checkout — has something to answer from, exactly like a real lane's
        # `.git` file pointing at its main checkout's `.git/worktrees/<lane>`.
        subprocess.run(["git", "init", "-q", self.root], check=True,
                       env=dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1"))
        os.makedirs(os.path.join(self.root, "tools"))
        stub_path = os.path.join(self.root, "tools", "build-gc.sh")
        with open(stub_path, "w") as f:
            f.write(STUB)
        os.chmod(stub_path, 0o755)
        self.log = os.path.join(self.root, "gc.log")
        self.marker = os.path.join(self.root, "ran")

    def run_hook(self, env_extra=None):
        env = dict(os.environ)
        env.pop("PLX_GC_LOG", None)
        env["CLAUDE_PROJECT_DIR"] = self.root
        env["PLX_GC_LOG"] = self.log
        if env_extra:
            env.update(env_extra)
        t0 = time.monotonic()
        result = subprocess.run(["sh", HOOK], input="{}", env=env, text=True,
                                capture_output=True, timeout=10, cwd=self.root)
        elapsed = time.monotonic() - t0
        return result, elapsed

    def test_returns_almost_instantly_regardless_of_the_launched_job(self):
        result, elapsed = self.run_hook()
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        # A generous margin over the stub's own 2s sleep: this hook's OWN work (git rev-parse,
        # a few string ops, spawning the child) must be near-instant, not merely under the
        # harness's outer timeout. 1s leaves headroom for a loaded CI runner while still failing
        # hard if the hook ever starts waiting on the child instead of detaching from it.
        self.assertLess(elapsed, 1.0, f"hook took {elapsed:.2f}s — it must detach, not wait")

    def test_actually_launches_the_detached_job(self):
        self.run_hook()
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline and not os.path.exists(self.marker):
            time.sleep(0.1)
        self.assertTrue(os.path.exists(self.marker),
                        "the detached job never ran — the hook silently dropped it")
        with open(self.marker) as f:
            self.assertIn("--auto", f.read())

    def test_survives_a_missing_build_gc_script(self):
        # A checkout with no tools/build-gc.sh at all (a stray call, a corrupted checkout) must
        # not crash the hook — SessionEnd hooks fail open by contract, same as every other hook
        # in this project.
        os.remove(os.path.join(self.root, "tools", "build-gc.sh"))
        result, elapsed = self.run_hook()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertLess(elapsed, 1.0)

    def test_resolves_the_main_checkout_not_a_lane_about_to_be_deleted(self):
        # `.claude/worktrees/<lane>` is a real `git worktree add`, so its own `tools/build-gc.sh`
        # differs from the main checkout's — the hook must launch the MAIN copy (git
        # rev-parse --git-common-dir points back at it) even when invoked from inside the lane,
        # because a lane can be torn down the moment this session ends.
        lane = os.path.join(self.root, ".claude", "worktrees", "lane0")
        subprocess.run(["git", "-C", self.root, "-c", "user.email=t@t", "-c", "user.name=t",
                        "commit", "--allow-empty", "-q", "-m", "seed"], check=True)
        subprocess.run(["git", "-C", self.root, "branch", "-m", "main"], check=True)
        subprocess.run(["git", "-C", self.root, "worktree", "add", "-q", "-b", "lane0", lane],
                       check=True)
        lane_marker = os.path.join(lane, "ran")
        env = dict(os.environ)
        env.pop("PLX_GC_LOG", None)
        env["CLAUDE_PROJECT_DIR"] = lane
        env["PLX_GC_LOG"] = self.log
        result = subprocess.run(["sh", HOOK], input="{}", env=env, text=True,
                                capture_output=True, timeout=10, cwd=lane)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline and not os.path.exists(self.marker):
            time.sleep(0.1)
        self.assertTrue(os.path.exists(self.marker),
                        "launched from the lane rather than the main checkout")
        self.assertFalse(os.path.exists(lane_marker),
                         "the lane's own stub ran — that lane may be gone by the time it finishes")

    def test_falls_back_to_claude_project_dir_without_git(self):
        # No `.git` at all (git missing, or a checkout git cannot read): the documented fallback
        # is $CLAUDE_PROJECT_DIR itself, not silence.
        root = tempfile.mkdtemp(prefix="build-gc-auto-hook-nogit-")
        self.addCleanup(shutil.rmtree, root, ignore_errors=True)
        os.makedirs(os.path.join(root, "tools"))
        stub_path = os.path.join(root, "tools", "build-gc.sh")
        with open(stub_path, "w") as f:
            f.write(STUB)
        os.chmod(stub_path, 0o755)
        marker = os.path.join(root, "ran")
        env = dict(os.environ)
        env.pop("PLX_GC_LOG", None)
        env["CLAUDE_PROJECT_DIR"] = root
        env["PLX_GC_LOG"] = os.path.join(root, "gc.log")
        result = subprocess.run(["sh", HOOK], input="{}", env=env, text=True,
                                capture_output=True, timeout=10, cwd=root)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline and not os.path.exists(marker):
            time.sleep(0.1)
        self.assertTrue(os.path.exists(marker), "no fallback to CLAUDE_PROJECT_DIR without git")


if __name__ == "__main__":
    unittest.main()
