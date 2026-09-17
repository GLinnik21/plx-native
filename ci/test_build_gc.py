#!/usr/bin/env python3
"""Run the real GC only against disposable repositories, fleet trees and cache locks."""
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parent.parent
MODES = ("--incremental", "--orphans", "--lanes", "--cache", "--all")


class BuildGcTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="build-gc-tests-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.counter = 0

    def fixture(self, pid, pgid, pgrep_body=None):
        self.counter += 1
        root = self.root / str(self.counter)
        repo, fleet, cache, tools = [root / n for n in ("repo", "fleet", "cache", "bin")]
        for path in (repo / "tools", fleet, cache, tools):
            path.mkdir(parents=True)
        script = repo / "tools/build-gc.sh"
        script.write_bytes((ROOT / "tools/build-gc.sh").read_bytes())
        env = os.environ.copy()
        # Never inherit a caller's repository selection or build paths.
        for key in list(env):
            if key.startswith("GIT_") or key in ("CARGO_TARGET_DIR", "SIM_TDIR"):
                del env[key]
        env.update(PLX_FLEET_DIR=str(fleet), PLX_BUILD_CACHE=str(cache),
                   PLX_CACHE_MAX_DAYS="30", GIT_CONFIG_GLOBAL=os.devnull,
                   GIT_CONFIG_NOSYSTEM="1")
        subprocess.run(["git", "init", "-q", str(repo)], env=env, check=True)
        # Suppress unrelated compiler-named processes only. PGID checks use real pgrep;
        # PID checks remain the script's actual shell kill -0 builtin.
        pgrep = tools / "pgrep"
        body = pgrep_body or '[ "$1" = -x ] && exit 1\n'
        pgrep.write_text("#!/bin/sh\n" + body + "exec "
                         + shlex.quote(shutil.which("pgrep")) + ' "$@"\n')
        pgrep.chmod(0o755)
        env["PATH"] = str(tools) + os.pathsep + env.get("PATH", "")
        sentinels = []
        for directory in (repo / "rust-modules/target/debug/incremental",
                          fleet / "absent-lane/target/debug/incremental",
                          cache / "ffmpeg/synthetic-key"):
            directory.mkdir(parents=True)
            sentinel = directory / "sentinel"
            sentinel.write_text("synthetic build output\n")
            sentinels.append(sentinel)
        stamp = cache / "ffmpeg/synthetic-key/.last-used"
        stamp.touch()
        old = time.time() - 40 * 86400
        os.utime(stamp, (old, old))
        lock = cache / "ffmpeg/synthetic-key.lock"
        lock.mkdir()
        for name, value in (("pid", pid), ("pgid", pgid)):
            if value is not None:
                (lock / name).write_text(str(value) + "\n")
        os.utime(lock, (old, old))
        return repo, env, sentinels, lock

    def run_gc(self, fixture, *args):
        repo, env, _, _ = fixture
        return subprocess.run(["sh", "tools/build-gc.sh", *args], cwd=repo, env=env,
                              text=True, capture_output=True, timeout=20)

    def assert_live_refusal(self, pid, pgid):
        for mode in MODES:
            with self.subTest(mode=mode):
                fixture = self.fixture(pid, pgid)
                result = self.run_gc(fixture, mode)
                diagnostic = result.stdout + result.stderr
                self.assertTrue(all(p.exists() for p in fixture[2]), diagnostic)
                self.assertNotEqual(result.returncode, 0, diagnostic)
                self.assertIn("held FFmpeg cache lock", diagnostic)
                self.assertNotIn("not found", diagnostic)

    def test_live_pid_refuses_every_reclaim_before_deletion(self):
        self.assert_live_refusal(os.getpid(), 0)

    def test_live_pgid_without_pid_refuses_every_reclaim(self):
        # macOS pgrep excludes ancestors by default. Give the lock a controlled child
        # group instead; its stdin remains open until all refusal checks have finished.
        child = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.read()"],
                                 stdin=subprocess.PIPE, start_new_session=True)
        try:
            group = os.getpgid(child.pid)
            subprocess.run([shutil.which("pgrep"), "-g", str(group)],
                           check=True, stdout=subprocess.DEVNULL)
            self.assert_live_refusal(0, group)
        finally:
            child.stdin.close()
            child.wait(timeout=5)

    def test_dead_malformed_missing_and_zero_owners_can_be_reclaimed(self):
        child = subprocess.Popen(["sh", "-c", "exit 0"])
        child.wait(timeout=5)
        with self.assertRaises(ProcessLookupError):
            os.kill(child.pid, 0)
        for pid, pgid in ((child.pid, 0), ("malformed", "invalid"), (None, None), (0, 0)):
            with self.subTest(pid=pid, pgid=pgid):
                fixture = self.fixture(pid, pgid)
                result = self.run_gc(fixture, "--all")
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertTrue(all(not p.exists() for p in fixture[2]))
                self.assertFalse(fixture[3].exists(), "stale lock stranded")
                self.assertNotIn("not found", result.stderr)

    def test_dry_runs_never_mutate_trees_or_locks(self):
        for owner in (os.getpid(), 0):
            for mode in MODES:
                with self.subTest(owner=owner, mode=mode):
                    fixture = self.fixture(owner, 0)
                    root = fixture[0].parent
                    def snapshot():
                        return {str(p.relative_to(root)): (p.stat().st_mtime_ns,
                                p.read_bytes() if p.is_file() else None)
                                for p in root.rglob("*")}
                    before = snapshot()
                    result = self.run_gc(fixture, mode, "-n")
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertEqual(before, snapshot())
                    self.assertNotIn("not found", result.stderr)

    def test_empty_worktree_enumeration_refuses_every_reclaim(self):
        for mode in MODES:
            with self.subTest(mode=mode):
                fixture = self.fixture(0, 0)
                fake_git = fixture[0].parent / "bin/git"
                fake_git.write_text("#!/bin/sh\nexit 1\n")
                fake_git.chmod(0o755)
                result = self.run_gc(fixture, mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("cannot enumerate", result.stderr)
                self.assertTrue(all(p.exists() for p in fixture[2]))


    # A build in ONE checkout must not veto reclaiming every OTHER tree on the volume. The guard
    # used to be all-or-nothing: any `cargo`/`make` anywhere and the script deleted nothing. On a
    # machine running several sessions at once that condition is essentially always true, so the
    # tool could not run on the day the volume filled — measured 2026-09-17 at 5.0 GiB free with
    # 34.8 GiB of collectable trees sitting in idle lanes.
    # Report a synthetic `cargo` whose cwd names the checkout to protect. `lsof` is real, so the
    # mapping from a pid back to a checkout is the production one, not a stub.
    def pid_file_stub(self, path):
        return ('if [ "$1" = -x ]; then\n'
                '  [ "$2" = cargo ] || exit 1\n'
                '  cat ' + shlex.quote(str(path)) + '\n'
                '  exit 0\n'
                'fi\n')

    def test_live_checkout_is_spared_while_every_other_tree_is_reclaimed(self):
        pidfile = self.root / "live.pid"
        pidfile.write_text("0\n")
        fixture = self.fixture(0, 0, pgrep_body=self.pid_file_stub(pidfile))
        repo, _, sentinels, _ = fixture
        live = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.read()"],
                                cwd=repo, stdin=subprocess.PIPE)
        try:
            pidfile.write_text(str(live.pid) + "\n")
            result = self.run_gc(fixture, "--all")
            diagnostic = result.stdout + result.stderr
            self.assertEqual(result.returncode, 0, diagnostic)
            self.assertIn("in use, skipped", diagnostic)
            self.assertTrue(sentinels[0].exists(), "reclaimed a tree being built: " + diagnostic)
            for stranded in sentinels[1:]:
                self.assertFalse(stranded.exists(), "idle tree left behind: " + diagnostic)
        finally:
            live.stdin.close()
            live.wait(timeout=5)

    def test_live_external_lane_tree_survives_even_with_its_worktree_gone(self):
        # `fleet-plan` points a worker's CARGO_TARGET_DIR at $PLX_FLEET_DIR/<lane>, and the
        # documented teardown order is: remove the worktrees, then `--orphans`. A lane whose last
        # build is still running is then an external tree with no worktree — which is exactly what
        # `--orphans` is built to delete. Its cwd cannot name a checkout git still lists, so the
        # lane name has to carry the answer.
        fixture = self.fixture(0, 0, pgrep_body=self.pid_file_stub(self.root / "live.pid"))
        repo, _, sentinels, _ = fixture
        pidfile = self.root / "live.pid"
        pidfile.write_text("0\n")
        lane = Path(str(sentinels[1])).parents[3]   # <fleet>/absent-lane
        live = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.read()"],
                                cwd=lane, stdin=subprocess.PIPE)
        try:
            pidfile.write_text(str(live.pid) + "\n")
            result = self.run_gc(fixture, "--all")
            diagnostic = result.stdout + result.stderr
            self.assertEqual(result.returncode, 0, diagnostic)
            self.assertTrue(sentinels[1].exists(),
                            "deleted an external lane tree being built: " + diagnostic)
            self.assertFalse(sentinels[0].exists(), "idle tree left behind: " + diagnostic)
        finally:
            live.stdin.close()
            live.wait(timeout=5)

    def test_own_ancestors_never_protect_the_checkout_being_cleaned(self):
        # `make disk` runs this script from a `make` whose cwd IS the checkout to clean. That make
        # is blocked waiting on us, not compiling; counting it protects the very tree the user
        # asked to reclaim. The stub reports the whole ancestor chain as live `cargo` processes.
        stub = ('if [ "$1" = -x ]; then\n'
                '  [ "$2" = cargo ] || exit 1\n'
                '  p=$PPID; n=0\n'
                '  while [ "$p" -gt 1 ] && [ "$n" -lt 32 ]; do\n'
                '    echo "$p"\n'
                '    p=$(ps -o ppid= -p "$p" 2>/dev/null | tr -d " ")\n'
                '    case "$p" in ""|*[!0-9]*) p=0 ;; esac\n'
                '    n=$((n + 1))\n'
                '  done\n'
                '  exit 0\n'
                'fi\n')
        fixture = self.fixture(0, 0, pgrep_body=stub)
        repo, env, sentinels, _ = fixture
        # An intermediate shell, so an ancestor really does have the repository as its cwd.
        result = subprocess.run(["sh", "-c", "sh tools/build-gc.sh --all"], cwd=repo, env=env,
                                text=True, capture_output=True, timeout=20)
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertNotIn("in use, skipped", diagnostic)
        for stranded in sentinels:
            self.assertFalse(stranded.exists(), "ancestor mistaken for a builder: " + diagnostic)


class MakeCheckContractTests(unittest.TestCase):
    def test_host_check_runs_gc_regressions(self):
        lines = (ROOT / "Makefile").read_text().splitlines()
        start = next(i for i, line in enumerate(lines) if line.startswith("check:"))
        recipe = []
        for line in lines[start + 1:]:
            if line and not line.startswith(("\t", "#")):
                break
            recipe.append(line)
        self.assertIn("\tpython3 ci/test_build_gc.py", recipe)


if __name__ == "__main__":
    unittest.main()
