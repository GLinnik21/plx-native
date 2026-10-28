#!/usr/bin/env python3
"""Run the real GC only against disposable repositories, fleet trees and cache locks."""
import importlib.util
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

import check_steps

ROOT = Path(__file__).resolve().parent.parent


def git_has_merge_tree():
    """`git merge-tree --write-tree` (the squash check) arrived in git 2.38."""
    import re
    out = subprocess.run(["git", "--version"], capture_output=True, text=True).stdout
    m = re.search(r"(\d+)\.(\d+)", out)
    return bool(m) and (int(m.group(1)), int(m.group(2))) >= (2, 38)
MODES = ("--incremental", "--orphans", "--lanes", "--stale", "--cache", "--worktrees", "--all")

# HANG GUARD, NOT A SPEED BOUND. Every run of the script below ends at the child's own exit; this is
# only how long a test waits for a child that never exits before it gives up and fails. The honest cost
# of one run is CPU, not wall: an `--auto` is ~1.3 CPU-seconds (about 150 short-lived processes at ~2 ms
# of CPU each on an Apple M4, plus three python starts), and the whole file is ~65 CPU-seconds. Wall
# time is that CPU divided by the share the scheduler grants, so it follows the machine's load:
# measured on a Mac with load 45-65 from other lanes' builds, one `--auto` took 2.5-13 s and the
# slowest test 13 s, and a fixed 30 s bound failed every lane's `make check` whenever a load spike
# stretched one run past it. 600 s is far past any spike and still ends a genuine hang inside the
# `make check` step's own budget; do not tighten it to "catch slowness" - a slow run is a loaded
# machine, not a bug (test_a_hung_run_is_killed_with_everything_it_started is the proof a hang is
# still caught).
HANG_GUARD_SECS = 600


def run_guarded(argv, cwd, env, timeout=HANG_GUARD_SECS):
    """`subprocess.run(text=True, capture_output=True)` whose timeout actually ends the run.

    `subprocess.run(timeout=)` kills only the direct child and then waits for the pipes to close, but
    the script's stages run in `$(...)` and python children that inherit those pipes: a hung grandchild
    keeps them open and the "timeout" blocks forever in `communicate()`. The child is therefore started
    as its own session and process-group leader, and on timeout the whole group - which this call
    created, so the pgid is the pid it started - is killed before `TimeoutExpired` is raised.
    A shell can fork during the first group kill; retry with a bounded pipe drain instead of
    replacing the original timeout with an unbounded `communicate()`."""
    proc = subprocess.Popen(argv, cwd=cwd, env=env, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, stdin=subprocess.DEVNULL, start_new_session=True)
    try:
        out, err = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        for _ in range(10):
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except OSError:
                pass
            try:
                proc.communicate(timeout=1)
                break
            except subprocess.TimeoutExpired:
                continue
        else:
            # A pipe held outside the owned group must still not hang the entire test runner.
            proc.stdout.close()
            proc.stderr.close()
            proc.wait(timeout=1)
        raise
    return subprocess.CompletedProcess(argv, proc.returncode, out, err)


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
        # `tmproot` is the one place the planner's /tmp discovery looks (PLX_GC_TMP_ROOTS), and `scratch` is
        # this fixture's TMPDIR (beside the fixture rather than in it: the dry-run tests compare the whole
        # fixture before and after, and a run's own temp files come and go there). Neither is ever the
        # machine's real /tmp, whatever a test asks the clock or the free-space probe to pretend (the
        # planner refuses to plan under those while a root is left to its default).
        tmproot, scratch = root / "tmproot", self.root / ("scratch-%d" % self.counter)
        for path in (repo / "tools", fleet, cache, tools, tmproot, scratch):
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
                   GIT_CONFIG_NOSYSTEM="1",
                   # `--auto`'s log and lock live under THIS fixture's own root — never the real
                   # ~/Library/Logs or /tmp/plx-build-gc-auto.lock, which a concurrent real
                   # `--auto` (the SessionEnd hook, launchd) could be holding on the very machine
                   # running this suite.
                   PLX_GC_LOG=str(root / "gc.log"), PLX_GC_LOCK_DIR=str(root / "gc.lock"),
                   PLX_GC_TMP_ROOTS=str(tmproot), TMPDIR=str(scratch))
        for key in ("PLX_GC_TEST_NOW", "PLX_GC_TEST_FREE_KIB", "PLX_GC_IDLE_MIN", "PLX_GC_MIN_FREE_GIB"):
            env.pop(key, None)
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
        return run_guarded(["sh", "tools/build-gc.sh", *args], repo, env)

    def run_auto(self, fixture, *args, extra_env=None):
        repo, env, _, _ = fixture
        env = dict(env)
        if extra_env:
            env.update(extra_env)
        return run_guarded(["sh", "tools/build-gc.sh", "--auto", *args], repo, env)

    def test_an_auto_run_reads_the_process_table_once_per_ancestor_walk(self):
        # The walk up our own ancestors (so a `make disk` that is blocked on us is not mistaken for a
        # builder) used to run `ps -o ppid= -p PID` once per hop, in the shell's `live_checkouts` of every
        # stage and again in the planner's `ancestor_pids()`: 67 `ps` for one `--auto` here, more under a
        # deeper `make check` -> python -> sh chain. A process spawn costs ~2 ms of CPU and, under load,
        # many times that in wall time, so the count is the cost. This is a DESIGN assertion on the number
        # of spawns (one read of the process table per walk, whatever the depth), not a speed bound.
        fixture, _ = self.evict_fixture(lanes=1)
        repo, env, _, _ = fixture
        log = repo.parent / "ps.log"
        tools = Path(env["PATH"].split(os.pathsep)[0])
        stub = tools / "ps"
        stub.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> " + shlex.quote(str(log)) + "\nexec "
                        + shlex.quote(shutil.which("ps")) + ' "$@"\n')
        stub.chmod(0o755)
        result = self.run_auto(fixture, extra_env={"PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        calls = log.read_text().splitlines()
        per_hop = [c for c in calls if "ppid=" in c and "-p " in c]
        self.assertEqual(per_hop, [], "an ancestor walk spawned `ps` once per hop:\n" + "\n".join(calls[:12]))
        self.assertTrue(calls, "the walk never ran, so this test proves nothing: " + diagnostic)
        # one table read per walk: the stages (orphans, worktrees, evict) and the planner's probes
        self.assertLessEqual(len(calls), 10, "\n".join(calls))

    def test_a_hung_run_is_killed_with_everything_it_started(self):
        # The guard above is only worth having if a genuine hang still ends the test. `git` is the first
        # thing the script runs, so a `git` that never returns hangs it at once: the stub records its pid
        # and becomes a `sleep` that inherits the script's pipes (its stderr is the test's capture pipe),
        # the case where `subprocess.run(timeout=)` kills the shell and then waits on `communicate()`
        # forever. The guard must raise promptly AND leave nothing of the run alive.
        fixture = self.fixture(0, 0)
        repo, env, _, _ = fixture
        pidfile = repo.parent / "hung.pid"
        stub = Path(env["PATH"].split(os.pathsep)[0]) / "git"
        stub.write_text("#!/bin/sh\necho $$ > " + shlex.quote(str(pidfile)) + "\nexec sleep 3600\n")
        stub.chmod(0o755)
        started = time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired):
            run_guarded(["sh", "tools/build-gc.sh", "--auto"], repo, env, timeout=5)
        self.assertLess(time.monotonic() - started, 60, "the guard did not end the run")
        pid = int(pidfile.read_text())
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.05)
        else:
            os.kill(pid, signal.SIGKILL)  # do not leave our own hung child behind, then fail
            self.fail("the hung grandchild (pid %d) outlived the guard" % pid)

    def test_a_run_whose_grandchild_holds_the_pipes_is_still_ended_by_the_guard(self):
        # The same property without the script: a shell that backgrounds a sleeper holding its stdout.
        pidfile = self.root / "sleeper.pid"
        started = time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired):
            run_guarded(["sh", "-c", "sleep 3600 & echo $! > " + shlex.quote(str(pidfile)) + "; wait"],
                        self.root, os.environ.copy(), timeout=5)
        self.assertLess(time.monotonic() - started, 60)
        pid = int(pidfile.read_text())
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.05)
        os.kill(pid, signal.SIGKILL)
        self.fail("the sleeper (pid %d) outlived the guard" % pid)

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

    def test_cache_prunes_an_aged_cargo_seed_and_keeps_a_used_one(self):
        fixture = self.fixture(None, None)
        repo, env = fixture[0], fixture[1]
        shutil.copy(ROOT / "tools/cargo-seed.py", repo / "tools/cargo-seed.py")
        seeds = Path(env["PLX_BUILD_CACHE"]) / "cargo-seed"
        old = time.time() - 40 * 86400
        for name, mtime in (("target", old), ("target-release", time.time())):
            (seeds / name).mkdir(parents=True)
            (seeds / name / "payload").write_text("third-party output\n")
            stamp = seeds / name / ".last-used"
            stamp.touch()
            os.utime(stamp, (mtime, mtime))
        dry = self.run_gc(fixture, "--cache", "-n")
        self.assertEqual(dry.returncode, 0, dry.stdout + dry.stderr)
        self.assertIn("would remove", dry.stdout)
        self.assertTrue((seeds / "target/payload").exists(), "a dry run deleted the seed")
        real = self.run_gc(fixture, "--cache")
        self.assertEqual(real.returncode, 0, real.stdout + real.stderr)
        self.assertFalse((seeds / "target").exists(), "an aged seed survived --cache")
        self.assertTrue((seeds / "target-release/payload").exists(), "a seed in use was pruned")

    def test_seed_prunes_a_week_old_cargo_seed_that_cache_would_keep(self):
        fixture = self.fixture(None, None)
        repo, env = fixture[0], fixture[1]
        shutil.copy(ROOT / "tools/cargo-seed.py", repo / "tools/cargo-seed.py")
        seeds = Path(env["PLX_BUILD_CACHE"]) / "cargo-seed"
        (seeds / "target").mkdir(parents=True)
        (seeds / "target" / "payload").write_text("third-party output\n")
        stamp = seeds / "target" / ".last-used"
        stamp.touch()
        ten_days = time.time() - 10 * 86400
        os.utime(stamp, (ten_days, ten_days))
        kept = self.run_gc(fixture, "--cache")
        self.assertEqual(kept.returncode, 0, kept.stdout + kept.stderr)
        self.assertTrue((seeds / "target/payload").exists(), "--cache pruned a 10-day-old seed")
        real = self.run_gc(fixture, "--seed")
        self.assertEqual(real.returncode, 0, real.stdout + real.stderr)
        self.assertFalse((seeds / "target").exists(), "--seed left a seed unused for 10 days")

    def test_auto_under_pressure_never_evicts_the_cargo_seed(self):
        # The lanes cloned from the seed share its blocks, so deleting it frees almost nothing and makes
        # every new lane's first build cold: neither `--auto` nor the planner ever takes it (`--seed`, by hand,
        # still does: test_seed_prunes_a_week_old_cargo_seed_that_cache_would_keep).
        fixture = self.with_planner(self.fixture(0, 0))
        repo, env = fixture[0], fixture[1]
        seeds = Path(env["PLX_BUILD_CACHE"]) / "cargo-seed"
        (seeds / "target").mkdir(parents=True)
        (seeds / "target" / "payload").write_text("third-party output\n")
        stamp = seeds / "target" / ".last-used"
        stamp.touch()
        ten_days = time.time() - 10 * 86400
        os.utime(stamp, (ten_days, ten_days))
        result = self.run_auto(fixture, extra_env={
            "PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999", "PLX_GC_IDLE_MIN": "0"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((seeds / "target/payload").exists(), "--auto under pressure evicted the seed")
        log = Path(env["PLX_GC_LOG"]).read_text()
        self.assertNotIn("seed", log.replace("cargo-seed", ""), "a seed stage ran:\n" + log)

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

    # Regression for a `set -e` + pipeline-subshell bug: every derived-tree helper
    # (`vendor_trees`, `lane_trees`, `incremental_trees`, `external_trees`, ...) used to end its
    # `for` loop body in `[ -d "$d" ] && echo "$d"`. When the LAST glob candidate did not exist —
    # true of every repo here, since none of these fixtures create a `vendor/` dir — that line's
    # exit status was non-zero, which became the function's own return status. A bare call to a
    # function shaped like that (`lane_trees "$w"` as the final statement of a
    # `worktrees | while read w; do ...; done` loop body) is NOT exempt from `set -e`, so the
    # `while` loop's own subshell exited the moment it hit the first worktree — silently
    # truncating `--lanes`/`--incremental`/`--worktrees` to their first entry, with exit code 0
    # and no error printed. Real repo measured 2026-09-18: `--lanes -n` listed nothing while a
    # linked worktree's `rust-modules/target` alone was 3.1 GB.
    def _add_worktrees(self, fixture, n, prefix="lane", add_target=True):
        repo, env, _, _ = fixture
        (repo / "README").write_text("seed\n")
        subprocess.run(["git", "-C", str(repo), "add", "README"], env=env, check=True)
        subprocess.run(["git", "-C", str(repo), "-c", "user.email=t@t", "-c", "user.name=t",
                        "commit", "-q", "-m", "seed"], env=env, check=True)
        # `worktree_reason()` (the `--worktrees` mode) tests ancestry against the literal ref
        # `main`; `git init` here has no global `init.defaultBranch` to read (the fixture strips
        # GIT_CONFIG_GLOBAL) and falls back to `master`. Rename so both modes see a real `main`.
        subprocess.run(["git", "-C", str(repo), "branch", "-m", "main"], env=env, check=True)
        worktree_roots = []
        for i in range(n):
            wt = repo.parent / f"{prefix}{i}"
            subprocess.run(["git", "-C", str(repo), "worktree", "add", "-q", "-b",
                            f"{prefix}{i}", str(wt)], env=env, check=True)
            if add_target:
                tgt = wt / "rust-modules/target"
                tgt.mkdir(parents=True)
                (tgt / "sentinel").write_text("synthetic build output\n")
            worktree_roots.append(wt)
        return worktree_roots

    def test_lanes_enumerates_every_worktree_not_just_the_first(self):
        fixture = self.fixture(0, 0)
        roots = self._add_worktrees(fixture, 3)
        result = self.run_gc(fixture, "--lanes", "-n")
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        for wt in roots:
            tgt = wt / "rust-modules/target"
            self.assertIn(str(tgt), diagnostic,
                          "worktree target missing from --lanes output: " + diagnostic)

    def test_the_managed_hostsim_tree_is_reclaimed_like_every_other_target_dir(self):
        # `make test-hostsim` builds into `rust-modules/target-hostsim` (it replaced a hand recipe
        # whose `/tmp/<dir>` trees nothing tracked). The derived-tree helpers glob `target*` rather
        # than list names, so the dir needs no entry here -- this pins that, for both the narrow mode
        # (`--incremental`: the cache only, the compiled output stays) and the whole-tree one
        # (`--lanes`), in a linked worktree.
        fixture = self.fixture(0, 0)
        (lane,) = self._add_worktrees(fixture, 1, add_target=False)
        tree = lane / "rust-modules/target-hostsim/debug"
        for sub in ("incremental", "deps"):
            (tree / sub).mkdir(parents=True)
            (tree / sub / "sentinel").write_text("synthetic build output\n")
        listed = self.run_gc(fixture, "--lanes", "-n")
        self.assertIn(str(lane / "rust-modules/target-hostsim"), listed.stdout + listed.stderr)
        narrow = self.run_gc(fixture, "--incremental")
        self.assertEqual(narrow.returncode, 0, narrow.stdout + narrow.stderr)
        self.assertFalse((tree / "incremental/sentinel").exists(), "the incremental cache was left behind")
        self.assertTrue((tree / "deps/sentinel").exists(), "--incremental reached past the cache")
        whole = self.run_gc(fixture, "--lanes")
        self.assertEqual(whole.returncode, 0, whole.stdout + whole.stderr)
        self.assertFalse((lane / "rust-modules/target-hostsim").exists(), "the lane's tree was left behind")

    def test_worktrees_mode_enumerates_past_the_first_record(self):
        # No target dir here: an untracked build tree would make every worktree read `dirty`,
        # which is a real (and correctly refused) state but not what this test is checking. This
        # test asks whether `--worktrees` enumerates past the first CLEAN, already-on-`main`
        # worktree — so every worktree here is left exactly at the seed commit.
        fixture = self.fixture(0, 0)
        roots = self._add_worktrees(fixture, 3, prefix="finished", add_target=False)
        result = self.run_gc(fixture, "--worktrees", "-n")
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        for wt in roots:
            self.assertIn(f"would remove  {wt}", diagnostic,
                          "worktree missing from --worktrees output: " + diagnostic)

    def test_worktrees_removal_guards_empty_fleet_dir(self):
        # Copilot review point: `FLEET_DIR=${PLX_FLEET_DIR-$HOME/plx-fleet}` only substitutes the
        # default when PLX_FLEET_DIR is UNSET. A caller that exports it as the EMPTY STRING
        # leaves FLEET_DIR empty, and the old code built `ext="$FLEET_DIR/$(basename "$w")"`
        # unconditionally — a ROOT-level path such as `/lane0`. Proving this safely, without ever
        # creating or testing a real root-level directory, means watching CONTROL FLOW rather
        # than a filesystem effect: `sh -x` traces every command it executes, including an
        # assignment and a `[ -d ... ]` test on a path that does not exist. Before the fix, the
        # trace shows `ext=/<name>` being built; after the fix, the guard
        # (`[ -n "$FLEET_DIR" ] && [ -d "$FLEET_DIR" ]`) is false and the whole block — including
        # the `ext=` assignment — never runs.
        fixture = self.fixture(0, 0)
        roots = self._add_worktrees(fixture, 1, prefix="lane", add_target=False)
        repo, env, _, _ = fixture
        env = dict(env, PLX_FLEET_DIR="")
        result = run_guarded(["sh", "-x", "tools/build-gc.sh", "--worktrees"], repo, env)
        trace = result.stdout + result.stderr
        name = roots[0].name
        self.assertEqual(result.returncode, 0, trace)
        self.assertNotIn(f"ext=/{name}", trace,
                         "built a root-anchored external path from an empty PLX_FLEET_DIR: "
                         + trace)
        self.assertFalse(roots[0].exists(), "worktree was not actually removed: " + trace)

    # --- `--worktrees` reads the repository the way the planner does ---------------------------------
    def git(self, fixture, cwd, *args, check=True):
        return subprocess.run(["git", "-c", "user.email=t@t", "-c", "user.name=t", "-C", str(cwd), *args],
                              env=fixture[1], text=True, capture_output=True, check=check)

    def _squash_landed_lane(self, fixture, name, landed=True):
        """A lane with one commit of its own. `landed`: that commit reaches `origin/main` as ONE squash
        commit while the local `main` stays a commit BEHIND it (the state every day-old checkout is in:
        `main` is only moved by a fetch-and-merge nobody ran)."""
        repo = fixture[0]
        (repo / "README").write_text("seed\n")
        # the repository's own ignore rules: a lane's `src/config.local.h` is IGNORED (a clean tree), not untracked
        (repo / ".gitignore").write_bytes((ROOT / ".gitignore").read_bytes())
        self.git(fixture, repo, "add", "README", ".gitignore")
        self.git(fixture, repo, "commit", "-q", "-m", "seed")
        self.git(fixture, repo, "branch", "-m", "main")
        origin = repo.parent / "origin.git"
        subprocess.run(["git", "init", "-q", "--bare", "-b", "main", str(origin)], env=fixture[1], check=True)
        self.git(fixture, repo, "remote", "add", "origin", str(origin))
        self.git(fixture, repo, "push", "-q", "-u", "origin", "main")
        lane = repo.parent / name
        self.git(fixture, repo, "worktree", "add", "-q", "-b", name, str(lane))
        (lane / "work.txt").write_text(name + "\n")
        self.git(fixture, lane, "add", "work.txt")
        self.git(fixture, lane, "commit", "-q", "-m", name)
        if landed:
            self.git(fixture, repo, "merge", "-q", "--squash", name)
            self.git(fixture, repo, "commit", "-q", "-m", "squash " + name)
            self.git(fixture, repo, "push", "-q", "origin", "main")
            self.git(fixture, repo, "reset", "-q", "--hard", "HEAD~1")
        return lane

    @unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for merge-tree --write-tree")
    def test_worktrees_judges_a_lane_against_origin_main_not_a_stale_local_main(self):
        fixture = self.fixture(0, 0)
        landed = self._squash_landed_lane(fixture, "landed")
        open_lane = fixture[0].parent / "open"
        self.git(fixture, fixture[0], "worktree", "add", "-q", "-b", "open", str(open_lane))
        (open_lane / "wip.txt").write_text("not on origin\n")
        self.git(fixture, open_lane, "add", "wip.txt")
        self.git(fixture, open_lane, "commit", "-q", "-m", "wip")
        result = self.run_gc(fixture, "--worktrees", "-n")
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("would remove  %s" % landed, diagnostic,
                      "a lane squash-merged into origin/main read as unmerged against the stale local main")
        self.assertRegex(diagnostic, r"unmerged\s+%s" % open_lane)

    @unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for merge-tree --write-tree")
    def test_worktrees_falls_back_to_the_local_main_without_an_origin(self):
        fixture = self.fixture(0, 0)
        roots = self._add_worktrees(fixture, 1, prefix="finished", add_target=False)
        result = self.run_gc(fixture, "--worktrees", "-n")
        self.assertIn("would remove  %s" % roots[0], result.stdout + result.stderr)

    def test_worktrees_checks_never_refresh_a_lanes_index(self):
        # `git status` rewrites the index when a tracked file's stat data went stale (a `touch`, a
        # checkout, a restore) unless GIT_OPTIONAL_LOCKS=0: a "report" that rewrites a lane's index
        # races that lane's own `git add`. The control proves this test can see the write.
        fixture = self.fixture(0, 0)
        roots = self._add_worktrees(fixture, 1, prefix="lane", add_target=False)
        lane = roots[0]
        index = Path(self.git(fixture, lane, "rev-parse", "--path-format=absolute", "--git-path",
                              "index").stdout.strip())
        later = time.time() + 60
        os.utime(lane / "README", (later, later))
        before = (index.read_bytes(), index.stat().st_mtime_ns)
        control_env = dict(fixture[1])
        subprocess.run(["git", "-C", str(lane), "status", "--porcelain"], env=control_env, check=True,
                       capture_output=True)
        self.assertNotEqual(before, (index.read_bytes(), index.stat().st_mtime_ns),
                            "the control `git status` did not refresh the index: this test cannot fail")
        os.utime(lane / "README", (later + 60, later + 60))
        before = (index.read_bytes(), index.stat().st_mtime_ns)
        result = self.run_gc(fixture, "--worktrees", "-n")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(before, (index.read_bytes(), index.stat().st_mtime_ns),
                         "build-gc refreshed a lane's index")

    @unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for merge-tree --write-tree")
    def test_worktrees_squash_check_adds_no_object_to_the_repository(self):
        # `git merge-tree --write-tree` WRITES the merged tree (and any merged blob) into the object store.
        # `--worktrees` runs it for every lane that is not an ancestor of origin/main, and `--auto` runs
        # `--worktrees` hourly: against the real store that is a loose object per unmerged lane per change
        # of either side, until a `git gc`. The check therefore runs against a throwaway object directory
        # with the real one as an alternate, as `disk-budget.py` does. The control proves the count moves.
        fixture = self.fixture(0, 0)
        repo = fixture[0]
        lane = self._squash_landed_lane(fixture, "open", landed=False)
        (repo / "other.txt").write_text("trunk moved on\n")
        self.git(fixture, repo, "add", "other.txt")
        self.git(fixture, repo, "commit", "-q", "-m", "trunk moves")
        self.git(fixture, repo, "push", "-q", "origin", "main")        # the merge now yields a NEW tree

        def loose():
            out = self.git(fixture, repo, "count-objects", "-v").stdout
            return int(next(l.split()[1] for l in out.splitlines() if l.startswith("count:")))
        before = loose()
        result = self.run_gc(fixture, "--worktrees", "-n")
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertRegex(diagnostic, r"unmerged\s+%s" % lane)         # the squash check did run, and said no
        self.assertEqual(before, loose(), "build-gc wrote an object into the repository: " + diagnostic)
        self.git(fixture, lane, "merge-tree", "--write-tree", "origin/main", "open")
        self.assertGreater(loose(), before, "the control merge-tree wrote nothing: this test cannot fail")

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
        result = run_guarded(["sh", "-c", "sh tools/build-gc.sh --all"], repo, env)
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertNotIn("in use, skipped", diagnostic)
        for stranded in sentinels:
            self.assertFalse(stranded.exists(), "ancestor mistaken for a builder: " + diagnostic)

    # --- `--stale` ---------------------------------------------------------------------------
    # A synthetic `deps/` directory with two metadata hashes of ONE crate: an OLD, superseded
    # hash (binary + .d + two `.rcgu.o` objects, all 30h old) and the NEWEST hash (binary + .d
    # fresh, its two `.rcgu.o` objects independently aged past the threshold). The main fixture
    # repo IS the "main checkout" as far as build-gc.sh is concerned (no worktrees added), so this
    # exercises the conservative age-only rule: the old hash is deleted outright, the newest
    # hash's binary and `.d` survive untouched, and only ITS stale objects are declined.
    def _stale_deps_fixture(self):
        fixture = self.fixture(0, 0)
        repo, _, _, _ = fixture
        deps = repo / "rust-modules/target/debug/deps"
        deps.mkdir(parents=True)
        now = time.time()
        old = now - 30 * 3600
        fresh = now - 1 * 3600

        def touch(name, mtime, executable=False):
            path = deps / name
            path.write_bytes(b"")
            if executable:
                path.chmod(0o755)
            os.utime(path, (mtime, mtime))
            return path

        old_files = [
            touch("plxnative_modules-aaaaaaaaaaaaaaaa", old, executable=True),
            touch("plxnative_modules-aaaaaaaaaaaaaaaa.d", old),
            touch("plxnative_modules-aaaaaaaaaaaaaaaa.codehash-cgu.00.rcgu.o", old),
            touch("plxnative_modules-aaaaaaaaaaaaaaaa.codehash-cgu.01.rcgu.o", old),
        ]
        newest_core = [
            touch("plxnative_modules-bbbbbbbbbbbbbbbb", fresh, executable=True),
            touch("plxnative_modules-bbbbbbbbbbbbbbbb.d", fresh),
        ]
        newest_stale_objects = [
            touch("plxnative_modules-bbbbbbbbbbbbbbbb.codehash-cgu.00.rcgu.o", old),
            touch("plxnative_modules-bbbbbbbbbbbbbbbb.codehash-cgu.01.rcgu.o", old),
        ]
        return fixture, old_files, newest_core, newest_stale_objects

    def test_stale_preview_reports_superseded_hash_and_declined_objects_without_deleting(self):
        fixture, old_files, newest_core, newest_stale_objects = self._stale_deps_fixture()
        result = self.run_gc(fixture, "--stale", "-n")
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("would remove", diagnostic)
        self.assertIn("plxnative_modules-aaaaaaaaaaaaaaaa (4 files)", diagnostic)
        self.assertIn("plxnative_modules-bbbbbbbbbbbbbbbb rcgu.o objects (2 files)", diagnostic)
        for p in old_files + newest_core + newest_stale_objects:
            self.assertTrue(p.exists(), "-n deleted a file: " + diagnostic)

    def test_stale_deletes_superseded_hash_keeps_newest_binary_declines_its_old_objects(self):
        fixture, old_files, newest_core, newest_stale_objects = self._stale_deps_fixture()
        result = self.run_gc(fixture, "--stale")
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        for p in old_files:
            self.assertFalse(p.exists(), "superseded hash survived --stale: " + diagnostic)
        for p in newest_stale_objects:
            self.assertFalse(p.exists(), "stale object of the kept hash survived: " + diagnostic)
        for p in newest_core:
            self.assertTrue(p.exists(), "the newest hash's binary/.d was deleted: " + diagnostic)

    def test_stale_keeps_a_hash_younger_than_the_threshold_even_if_superseded(self):
        # A THIRD hash, younger than the (overridden, tiny) threshold: even though it is neither
        # the newest for its crate nor old enough, it must survive — the "keep anything younger
        # than $PLX_GC_STALE_HOURS" clause is independent of the newest-hash rule.
        fixture, old_files, newest_core, newest_stale_objects = self._stale_deps_fixture()
        repo, env, _, _ = fixture
        deps = repo / "rust-modules/target/debug/deps"
        young_but_superseded = deps / "plxnative_modules-cccccccccccccccc"
        young_but_superseded.write_bytes(b"")
        young_but_superseded.chmod(0o755)
        recent = time.time() - 2 * 3600
        os.utime(young_but_superseded, (recent, recent))
        env = dict(env, PLX_GC_STALE_HOURS="3")
        result = run_guarded(["sh", "tools/build-gc.sh", "--stale"], repo, env)
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertTrue(young_but_superseded.exists(),
                        "a hash younger than the threshold was reclaimed: " + diagnostic)
        for p in old_files:
            self.assertFalse(p.exists(), "superseded hash survived --stale: " + diagnostic)

    # --- `--auto` ----------------------------------------------------------------------------
    # `PLX_GC_TEST_FREE_KIB` stands in for `df`, and `PLX_GC_MIN_FREE_GIB`/`PLX_GC_IDLE_MIN`
    # (both in the units the script itself takes) drive the staging and idle-guard decisions
    # without ever touching a real disk or a real clock.

    def test_auto_above_threshold_only_runs_orphans(self):
        # A huge free-space stub against a tiny threshold: every below-threshold stage must be
        # skipped, and only the always-on --orphans stage may fire. sentinels[0] is the repo's own
        # (== main checkout's) incremental cache, sentinels[1] is the orphaned external lane tree,
        # sentinels[2] is a stale FFmpeg cache entry --auto must never touch (no --cache stage).
        fixture = self.fixture(0, 0)
        result = self.run_auto(fixture, extra_env={
            "PLX_GC_TEST_FREE_KIB": "999999999", "PLX_GC_MIN_FREE_GIB": "1"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("already above threshold", diagnostic)
        _, _, sentinels, _ = fixture
        self.assertTrue(sentinels[0].exists(), "touched the main checkout above threshold: " + diagnostic)
        self.assertFalse(sentinels[1].exists(), "orphans must run even above threshold: " + diagnostic)
        self.assertTrue(sentinels[2].exists(), "--auto must never run --cache: " + diagnostic)

    def auto_lanes(self, lanes=2, age=2 * 3600):
        """Idle lanes with build output (and the planner next to the script), plus an old main checkout."""
        fixture, roots = self.evict_fixture(lanes=lanes)
        for wt in roots:
            self.age(wt, age)
        self.age(fixture[0] / "rust-modules/target", age)
        self.age(fixture[1]["PLX_FLEET_DIR"], age)
        return fixture, roots

    def test_auto_below_threshold_follows_the_planners_order(self):
        # 10 GiB free against a 12 GiB threshold: below it, but above the 3 GiB reserve. The stop line (16 GiB)
        # is never reached on a stubbed volume, so the whole of rungs 0-5 runs, in the planner's order, and
        # the budget-critical rung 6 (the main checkout's incremental cache) does not.
        fixture, roots = self.auto_lanes()
        repo, env, sentinels, _ = fixture
        result = self.run_auto(fixture, extra_env={
            "PLX_GC_TEST_FREE_KIB": str(10 << 20), "PLX_GC_MIN_FREE_GIB": "12", "PLX_GC_IDLE_MIN": "30"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        for wt in roots:
            self.assertFalse((wt / "rust-modules/target").exists(), "idle lane target survived: " + diagnostic)
        self.assertFalse(sentinels[1].exists(), "the orphan survived: " + diagnostic)
        self.assertTrue(sentinels[0].exists(), "rung 6 ran without being needed: " + diagnostic)
        self.assertTrue(sentinels[2].exists(), "--auto must never touch the FFmpeg cache: " + diagnostic)
        log = Path(env["PLX_GC_LOG"]).read_text()
        order = [m for m in ("orphans:", "worktrees (idle >= 30m):", "evict: rung 2 incremental",
                             "evict: rung 4 tree", "evict: --stale sweep at rung 5", "evict (planner order):")
                 if m in log]
        self.assertEqual(order, ["orphans:", "worktrees (idle >= 30m):", "evict: rung 2 incremental",
                                 "evict: rung 4 tree", "evict: --stale sweep at rung 5", "evict (planner order):"],
                         "stages ran out of order:\n" + log)
        for gone in (" incremental:", " lanes", " stale:", " seed:"):
            self.assertNotIn(gone, log, "a fixed stage of the old --auto ran:\n" + log)

    def test_auto_below_the_reserve_reaches_the_main_checkouts_incremental_cache_last(self):
        fixture, roots = self.auto_lanes()
        repo, env, sentinels, _ = fixture
        result = self.run_auto(fixture, extra_env={
            "PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "12", "PLX_GC_IDLE_MIN": "30"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertFalse(sentinels[0].exists(), "the last rung did not run under a full disk: " + diagnostic)
        self.assertTrue((repo / "rust-modules/target").is_dir(), "took the main checkout's whole target: " + diagnostic)
        log = Path(env["PLX_GC_LOG"]).read_text()
        self.assertLess(log.index("evict: rung 4 tree"), log.index("evict: rung 6 incremental"), log)

    def test_auto_idle_guard_spares_a_recently_touched_lane(self):
        # The DEFAULT idle window (60 minutes): a lane `evict_fixture` just built in has output written seconds
        # ago, so the planner does not offer it and nothing evicts it, however far below threshold the disk is.
        # A stopped agent might resume in the next few minutes; a needless rebuild is real money.
        fixture, roots = self.evict_fixture(lanes=1)
        result = self.run_auto(fixture, extra_env={
            "PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertTrue((roots[0] / "rust-modules/target/sentinel").exists(),
                        "a lane touched seconds ago was reclaimed: " + diagnostic)
        self.assertTrue((roots[0] / "rust-modules/target/debug/incremental").exists(), diagnostic)

    def test_auto_idle_guard_checks_the_external_tree_too(self):
        # A lane's `cargo build` writes into `$PLX_FLEET_DIR/<lane>/target`, not into the worktree, so a
        # worktree that has sat untouched past the idle window while its lane is mid-build (the only fresh
        # mtime is in the external tree) must still be spared.
        fixture, roots = self.evict_fixture(lanes=1)
        wt = roots[0]
        shutil.rmtree(wt / "rust-modules/target")
        self.age(wt, 3600)
        repo, env, sentinels, _ = fixture
        ext_target = self.make_tree(Path(env["PLX_FLEET_DIR"]) / wt.name / "target", incremental=True)
        result = self.run_auto(fixture, extra_env={
            "PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999", "PLX_GC_IDLE_MIN": "30"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertTrue((ext_target / "sentinel").exists(),
                        "external tree with a fresh write was reclaimed: " + diagnostic)
        self.assertTrue((ext_target / "debug/incremental").exists(), diagnostic)
        # the control: the same lane with its build tree old too is an idle lane, and goes
        self.age(ext_target.parent, 3600)
        result = self.run_auto(fixture, extra_env={
            "PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999", "PLX_GC_IDLE_MIN": "30"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(ext_target.exists(), "an idle lane's external tree was kept: " + result.stdout + result.stderr)

    @unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for merge-tree --write-tree")
    def test_auto_worktrees_stage_leaves_a_landed_lane_that_is_not_idle(self):
        # `--worktrees` on its own removes every finished lane. Under `--auto` a lane that LOOKS finished is
        # not necessarily nobody's: a squash can land while an agent is still in the worktree. The window
        # covers the worktree and the lane's build tree under the fleet dir.
        fixture = self.with_planner(self.fixture(0, 0))
        landed = self._squash_landed_lane(fixture, "landed")
        fleet_tree = self.make_tree(Path(fixture[1]["PLX_FLEET_DIR"]) / "landed" / "target")
        self.age(landed, 2 * 3600)
        quiet = {"PLX_GC_TEST_FREE_KIB": str(10 << 20), "PLX_GC_MIN_FREE_GIB": "12", "PLX_GC_IDLE_MIN": "30"}
        # worktree old, its fleet build tree written just now
        result = self.run_auto(fixture, extra_env=quiet)
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("recently active, skipped", diagnostic)
        self.assertTrue(landed.exists() and fleet_tree.exists(), "removed a lane with a fresh build tree: " + diagnostic)
        # everything old: now it goes, with its fleet tree
        self.age(fleet_tree.parent, 2 * 3600)
        result = self.run_auto(fixture, extra_env=quiet)
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertFalse(landed.exists(), "an idle landed lane was kept: " + diagnostic)
        self.assertFalse(fleet_tree.exists(), "its fleet tree was kept: " + diagnostic)
        # and `--worktrees` by hand has no idle window at all
        by_hand = self.fixture(0, 0)
        fresh = self._squash_landed_lane(by_hand, "landed")
        result = self.run_gc(by_hand, "--worktrees")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(fresh.exists(), "--worktrees by hand kept a landed lane for being recent")

    # --- `--worktrees` and the files git IGNORES ------------------------------------------------------------
    # `git worktree remove` deletes IGNORED files with the checkout, and "working tree clean" cannot see them: a
    # lane can hold `src/config.local.h` (the real Plex token), `pkg/auth.json`, `.tv-host`, or its own notes and
    # captures. By hand that is the human's call (the preview lists them); unattended (`--auto`, hourly and from the
    # SessionEnd hook, and now reaching squash-merged lanes) a lane is removed only when everything ignored in it is
    # build output `make` regenerates.
    DERIVED_ONLY = ("rust-modules/target/sentinel", "rust-modules/target-sim/debug/x", "pkg/plxnative",
                    "pkg/.build-config", "vendor/downloads/ffmpeg.tar.xz", "tools/__pycache__/x.cpython-312.pyc",
                    ".DS_Store", "src/starfish.o")
    UNDER_PRESSURE = {"PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999", "PLX_GC_IDLE_MIN": "30"}

    def put_files(self, lane, *rels):
        for rel in rels:
            path = lane / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("synthetic\n")

    def quiet_landed_lane(self, fixture, *rels):
        """A squash-merged lane holding `rels` (all gitignored), untouched for two hours. No planner is installed
        beside the script, so `--auto` runs its `--worktrees` stage and then skips the planner pass."""
        lane = self._squash_landed_lane(fixture, "landed")
        self.put_files(lane, *rels)
        self.assertEqual(self.git(fixture, lane, "status", "--porcelain").stdout, "",
                         "the fixture lane is not clean: its files are not all ignored")
        self.age(lane, 2 * 3600)
        return lane

    @unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for merge-tree --write-tree")
    def test_auto_worktrees_stage_spares_a_landed_lane_that_holds_a_private_file(self):
        fixture = self.fixture(0, 0)
        lane = self.quiet_landed_lane(fixture, *self.DERIVED_ONLY, "src/config.local.h")
        result = self.run_auto(fixture, extra_env=self.UNDER_PRESSURE)
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertTrue((lane / "src/config.local.h").exists(), "--auto removed a lane holding a private file: " + out)
        self.assertRegex(out, r"private +%s" % re.escape(str(lane)))
        self.assertIn("src/config.local.h", out)
        self.assertNotRegex(out, r"dirty|recently active", "the lane was kept for another reason: " + out)
        for rel in ("rust-modules/target", "pkg/plxnative", "__pycache__", ".DS_Store"):
            self.assertNotIn(rel, out, "build output was listed as what kept the lane: " + out)
        log = Path(fixture[1]["PLX_GC_LOG"]).read_text()
        self.assertIn("src/config.local.h", log, "the log does not say which path kept the lane")
        self.assertIn(str(lane), log)
        # the preview agrees: it names the lane as kept and offers no removal of it
        preview = self.run_auto(fixture, "-n", extra_env=self.UNDER_PRESSURE)
        out = preview.stdout + preview.stderr
        self.assertRegex(out, r"private +%s" % re.escape(str(lane)))
        self.assertNotIn("would remove  %s" % lane, out)

    @unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for merge-tree --write-tree")
    def test_auto_worktrees_stage_removes_a_landed_lane_whose_ignored_files_are_all_build_output(self):
        # The control for the test above: the same lane without the private file goes, with everything `make`
        # regenerates in it. (This passes on the code before the rule too: it pins that the rule is not a veto
        # on every lane that has ever been built in.)
        fixture = self.fixture(0, 0)
        lane = self.quiet_landed_lane(fixture, *self.DERIVED_ONLY)
        result = self.run_auto(fixture, extra_env=self.UNDER_PRESSURE)
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertFalse(lane.exists(), "a lane holding only build output was kept: " + out)
        self.assertNotRegex(out, r"private +/", out)

    @unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for merge-tree --write-tree")
    def test_auto_worktrees_stage_spares_a_lane_whose_ignored_files_cannot_be_listed(self):
        # Fail closed: when git cannot say what is ignored there, the answer is not "nothing".
        fixture = self.fixture(0, 0)
        lane = self.quiet_landed_lane(fixture, *self.DERIVED_ONLY)
        real_git = shutil.which("git")
        fake = fixture[0].parent / "bin/git"
        fake.write_text('#!/bin/sh\nfor a in "$@"; do case "$a" in --ignored*) exit 129 ;; esac; done\n'
                        'exec ' + shlex.quote(real_git) + ' "$@"\n')
        fake.chmod(0o755)
        result = self.run_auto(fixture, extra_env=self.UNDER_PRESSURE)
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertTrue(lane.exists(), "--auto removed a lane whose ignored files it could not list: " + out)
        self.assertIn("could not list", Path(fixture[1]["PLX_GC_LOG"]).read_text())

    @unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for merge-tree --write-tree")
    def test_worktrees_by_hand_lists_the_private_files_it_takes_with_the_lane(self):
        fixture = self.fixture(0, 0)
        lane = self.quiet_landed_lane(fixture, *self.DERIVED_ONLY, "src/config.local.h", "pkg/auth.json")
        preview = self.run_gc(fixture, "--worktrees", "-n")
        out = preview.stdout + preview.stderr
        self.assertEqual(preview.returncode, 0, out)
        self.assertIn("would remove  %s  (landed)" % lane, out)     # the line `--auto`'s counter reads, unchanged
        for rel in ("src/config.local.h", "pkg/auth.json"):
            self.assertIn(rel, out, "the preview does not say %s goes with the lane" % rel)
        self.assertNotIn("pkg/plxnative", out)
        self.assertTrue(lane.exists())
        # a human typing `--worktrees` is asking for the removal, private files and all, and is told what went
        result = self.run_gc(fixture, "--worktrees")
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertFalse(lane.exists(), out)
        self.assertIn("src/config.local.h", out)

    @unittest.skipUnless(git_has_merge_tree(), "needs git >= 2.38 for merge-tree --write-tree")
    def test_every_gitignore_pattern_is_either_build_output_or_kept_as_private(self):
        # The classification of "build output" is a list in build-gc.sh; the source of truth for what is ignored is
        # .gitignore. This ties them: a pattern added to .gitignore must be decided here (build output, or kept)
        # or the lane that holds its sample path is spared forever and this test names it.
        spec = importlib.util.spec_from_file_location("oguard", ROOT / ".claude/hooks/outbound-guard.py")
        guard = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(guard)
        kept_patterns = {"src/config.local.h", "local.env", "tests/manifest.local.json", ".claude/worktrees/",
                         "sysroot/", "rust-poc/", ".abi-cache/", ".webos-ndk/", ".tv-mac", ".tv-host",
                         ".tv-dpad-pass", ".tv-remote-url", "pkg/diagnostics/", "pkg/auth.json", "pkg/lab.json",
                         "pkg/telemetry.local.json", "tests/fixtures/softfloat/arm.tbl", "**/plxnative-recordings/"}
        derived, kept = {}, {rel: rel for rel in guard.PRIVATE_FILES}      # listed form -> file to create
        for line in (ROOT / ".gitignore").read_text().splitlines():
            pat = line.strip()
            if not pat or pat.startswith(("#", "!")):
                continue
            sample = pat.strip("/").replace("**/", "").replace("*", "x")
            # a pattern ending in `/` matches a directory only: the sample is a file inside one
            (kept if pat in kept_patterns else derived)[sample] = sample + "/f" if pat.endswith("/") else sample
        for rel in kept:
            derived.pop(rel, None)
        self.assertTrue(len(derived) > 30 and len(kept) > 15, (len(derived), len(kept)))
        # (1) a lane holding a sample of every build-output pattern is removable, and the preview has nothing to add
        fixture = self.fixture(0, 0)
        lane = self.quiet_landed_lane(fixture, *derived.values())
        preview = self.run_gc(fixture, "--worktrees", "-n")
        out = preview.stdout + preview.stderr
        self.assertIn("would remove  %s  (landed)" % lane, out)
        self.assertNotIn("not rebuildable", out, "build output was listed as private: " + out)
        auto = self.run_auto(fixture, extra_env=self.UNDER_PRESSURE)
        self.assertFalse(lane.exists(), "--auto kept a lane holding only .gitignore'd build output: "
                         + auto.stdout + auto.stderr)
        # (2) every private pattern, and every file outbound-guard.py guards, is listed by itself
        fixture = self.fixture(0, 0)
        lane = self.quiet_landed_lane(fixture, *kept.values())
        preview = self.run_gc(fixture, "--worktrees", "-n")
        out = preview.stdout + preview.stderr
        for rel in kept:
            self.assertRegex(out, r"(?m)^ +%s$" % re.escape(rel), "%s would go with the lane, unlisted: %s" % (rel, out))
        auto = self.run_auto(fixture, extra_env=self.UNDER_PRESSURE)
        self.assertTrue(lane.exists(), "--auto removed a lane holding private files: " + auto.stdout + auto.stderr)

    def test_auto_lock_held_exits_quietly(self):
        # A genuinely live owner (this TEST PROCESS's own pid, guaranteed alive for the duration
        # of the call) holding the lock must make a second --auto a silent no-op: exit 0, nothing
        # reclaimed, no refusal noise — the contract that lets a hook and a launchd tick overlap
        # without racing each other.
        fixture = self.fixture(0, 0)
        repo, env, sentinels, _ = fixture
        lock_dir = Path(env["PLX_GC_LOCK_DIR"])
        lock_dir.mkdir(parents=True)
        (lock_dir / "pid").write_text(str(os.getpid()) + "\n")
        result = self.run_auto(fixture, extra_env={
            "PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertTrue(sentinels[1].exists(), "reclaimed while another --auto held the lock: " + diagnostic)
        log = Path(env["PLX_GC_LOG"])
        if log.exists():
            self.assertIn("lock held", log.read_text())

    def test_auto_dry_run_deletes_nothing_and_leaves_no_lock(self):
        fixture, roots = self.auto_lanes(lanes=1)
        result = self.run_auto(fixture, "-n", extra_env={
            "PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999",
            "PLX_GC_IDLE_MIN": "30"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("would remove", diagnostic)
        self.assertRegex(diagnostic, r"would remove +~\S+ +\S+lane0/rust-modules/target  \(rung 4 tree, from lane0\)")
        self.assertTrue((roots[0] / "rust-modules/target").exists(),
                        "-n deleted a lane target: " + diagnostic)
        _, env, sentinels, _ = fixture
        for s in sentinels:
            self.assertTrue(s.exists(), "-n deleted a sentinel: " + diagnostic)
        self.assertFalse(Path(env["PLX_GC_LOCK_DIR"]).exists(),
                         "-n took the single-instance lock: " + diagnostic)


    # --- `--evict PLAN` ------------------------------------------------------------------------------------
    # The only mode that deletes what the planner chose. A plan is UNTRUSTED input, so most of what follows
    # hands it paths the planner would never write and checks that none of them goes. Everything lives under
    # the fixture's own root; `evict` is never pointed at this machine's trees.
    PLAN_V1 = "# plx-disk-plan v1"

    def with_planner(self, fixture):
        """The planner and the tools it loads, next to the copy of build-gc.sh under test."""
        repo = fixture[0]
        for name in ("disk-budget.py", "cargo-seed.py", "check-lock.py"):
            shutil.copy(ROOT / "tools" / name, repo / "tools" / name)
        return fixture

    def age(self, path, seconds):
        """Backdate `path` and everything under it (mtime and atime; the ctime is always now)."""
        old = time.time() - seconds
        for p in [path, *Path(path).rglob("*")]:
            try:
                os.utime(p, (old, old), follow_symlinks=False)
            except OSError:
                pass

    def make_tree(self, path, files=("sentinel",), incremental=False):
        path = Path(path)
        path.mkdir(parents=True, exist_ok=True)
        for f in files:
            (path / f).write_text("synthetic build output\n")
        if incremental:
            (path / "debug/incremental").mkdir(parents=True, exist_ok=True)
            (path / "debug/incremental/work-product").write_text("incremental\n")
        return path

    def write_plan(self, fixture, lines, goal_kib=None, header=None):
        """`lines`: (kind, rung, scope, est_kib, owner, idle_min, mode, path) tuples."""
        out = [header if header is not None else self.PLAN_V1, "# verdict reclaim"]
        if goal_kib is not None:
            out.append("# goal_kib %d" % goal_kib)
        for fields in lines:
            out.append("\t".join(str(f) for f in fields))
        plan = fixture[0].parent / ("plan-%d.tsv" % self.counter)
        plan.write_text("\n".join(out) + "\n")
        return plan

    def run_evict(self, fixture, plan, *args, extra_env=None, cwd=None):
        repo, env, _, _ = fixture
        env = dict(env)
        if extra_env:
            env.update(extra_env)
        return run_guarded(["sh", "tools/build-gc.sh", "--evict", str(plan), *args], cwd or repo, env)

    def evict_fixture(self, lanes=2, pid=0, pgid=0, pgrep_body=None, planner=True):
        fixture = self.fixture(pid, pgid, pgrep_body=pgrep_body)
        roots = self._add_worktrees(fixture, lanes, prefix="lane", add_target=False)
        if planner:
            self.with_planner(fixture)
        for wt in roots:
            self.make_tree(wt / "rust-modules/target", incremental=True)
        return fixture, roots

    def snapshot_tree(self, root):
        return {str(p.relative_to(root)): (p.read_bytes() if p.is_file() else None)
                for p in Path(root).rglob("*")}

    def test_evict_deletes_exactly_what_the_plan_names_and_logs_who_gave_it_up(self):
        fixture, (lane0, lane1) = self.evict_fixture()
        repo, env, sentinels, _ = fixture
        orphan = sentinels[1].parents[2]                      # <fleet>/absent-lane/target
        self.assertEqual(orphan.name, "target")
        (lane0 / "pkg").mkdir()
        (lane0 / "pkg/keep").write_text("pkg\n")
        (lane0 / "vendor/ffmpeg-build").mkdir(parents=True)
        (lane0 / "vendor/ffmpeg-build/keep").write_text("vendor\n")
        plan = self.write_plan(fixture, [
            ("evict", 0, "tree", 64, "-", 0, "normal", orphan),
            ("evict", 2, "incremental", 32, "lane0", 0, "normal", lane0 / "rust-modules/target/debug/incremental"),
            ("evict", 4, "tree", 96, "lane1", 0, "normal", lane1 / "rust-modules/target"),
        ])
        result = self.run_evict(fixture, plan)
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertFalse(orphan.exists(), out)
        self.assertFalse((lane0 / "rust-modules/target/debug/incremental").exists(), out)
        self.assertFalse((lane1 / "rust-modules/target").exists(), out)
        self.assertTrue((lane0 / "rust-modules/target/sentinel").exists(),
                        "took more than the incremental dir the plan named: " + out)
        for kept in (sentinels[0], sentinels[2], lane0 / "pkg/keep", lane0 / "vendor/ffmpeg-build/keep",
                     repo / "README"):
            self.assertTrue(kept.exists(), "evicted something the plan did not name (%s): %s" % (kept, out))
        self.assertEqual([p.name for p in orphan.parent.parent.rglob("*.tombstone.*")], [],
                         "a tombstone was left behind")
        log = Path(env["PLX_GC_LOG"]).read_text()
        self.assertRegex(log, r"evict: rung 4 tree freed ~\S+ from lane1: ")
        self.assertRegex(log, r"evict: rung 2 incremental freed ~\S+ from lane0: ")

    def test_evict_skips_a_tree_that_turned_live_and_takes_the_rest(self):
        import fcntl
        fixture, (lane0, lane1) = self.evict_fixture()
        busy = lane0 / "rust-modules/target/debug/.cargo-lock"
        busy.write_text("")
        plan = self.write_plan(fixture, [
            ("evict", 4, "tree", 96, "lane0", 0, "normal", lane0 / "rust-modules/target"),
            ("evict", 4, "tree", 96, "lane1", 0, "normal", lane1 / "rust-modules/target"),
        ])
        fd = os.open(busy, os.O_RDWR)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)      # "a cargo is building here, right now"
            result = self.run_evict(fixture, plan)
        finally:
            os.close(fd)
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertIn("in use, skipped", out)
        self.assertTrue((lane0 / "rust-modules/target/sentinel").exists(), "evicted a tree under a cargo lock: " + out)
        self.assertFalse((lane1 / "rust-modules/target").exists(), "the idle lane's tree should have gone: " + out)

    def test_evict_asks_again_whether_a_builder_runs_in_the_lane(self):
        pidfile = self.root / "live-evict.pid"
        pidfile.write_text("0\n")
        fixture, (lane0, lane1) = self.evict_fixture(pgrep_body=self.pid_file_stub(pidfile))
        plan = self.write_plan(fixture, [
            ("evict", 4, "tree", 96, "lane0", 0, "normal", lane0 / "rust-modules/target"),
            ("evict", 4, "tree", 96, "lane1", 0, "normal", lane1 / "rust-modules/target"),
        ])
        live = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.read()"],
                                cwd=lane0, stdin=subprocess.PIPE)
        try:
            pidfile.write_text(str(live.pid) + "\n")
            result = self.run_evict(fixture, plan)
        finally:
            live.stdin.close()
            live.wait(timeout=5)
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertTrue((lane0 / "rust-modules/target/sentinel").exists(), "evicted under a live build: " + out)
        self.assertFalse((lane1 / "rust-modules/target").exists(), out)

    def test_evict_honours_the_idle_window_the_plan_names(self):
        fixture, (lane0, lane1) = self.evict_fixture()
        self.age(lane1, 2 * 3600)
        self.age(fixture[1]["PLX_FLEET_DIR"], 2 * 3600)
        plan = self.write_plan(fixture, [
            ("evict", 4, "tree", 96, "lane0", 30, "normal", lane0 / "rust-modules/target"),
            ("evict", 4, "tree", 96, "lane1", 30, "normal", lane1 / "rust-modules/target"),
        ])
        result = self.run_evict(fixture, plan)
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertIn("its lane changed within 30 min", out)
        self.assertTrue((lane0 / "rust-modules/target/sentinel").exists(), "evicted a lane touched seconds ago: " + out)
        self.assertFalse((lane1 / "rust-modules/target").exists(), "left an idle lane's tree: " + out)

    def test_evict_refuses_paths_that_are_not_derived_build_trees(self):
        fixture, (lane0, lane1) = self.evict_fixture()
        repo, env, sentinels, _ = fixture
        fleet, cache, tmproot = (Path(env["PLX_FLEET_DIR"]), Path(env["PLX_BUILD_CACHE"]),
                                 Path(env["PLX_GC_TMP_ROOTS"]))
        # a lane's `pkg/` and `vendor/`, and a tracked file inside a tree
        (lane0 / "pkg").mkdir()
        (lane0 / "pkg/keep").write_text("pkg\n")
        (lane0 / "vendor/ffmpeg-build").mkdir(parents=True)
        (lane0 / "vendor/ffmpeg-build/keep").write_text("vendor\n")
        tracked = lane1 / "rust-modules/target-tracked"
        self.make_tree(tracked, files=("tracked.txt",))
        self.git(fixture, lane1, "add", "-f", "rust-modules/target-tracked/tracked.txt")
        self.git(fixture, lane1, "-c", "commit.gpgsign=false", "commit", "-q", "-m", "tracked build output")
        # another project's cargo tree in the one /tmp root the planner searches
        foreign = tmproot / "other-project-target"
        self.make_tree(foreign)
        (foreign / "CACHEDIR.TAG").write_text("Signature: 8a477f597d28d172789f06886806bc55\n")
        (foreign / "debug/deps").mkdir(parents=True)
        (foreign / "debug/deps/libother_crate-1234.rlib").write_text("not ours\n")
        self.age(foreign, 3 * 86400)
        # a symlink resolving into the main checkout's target, and a path nowhere near a checkout
        link = lane0.parent / "alias-of-main-target"
        link.symlink_to(repo / "rust-modules/target", target_is_directory=True)
        elsewhere = self.make_tree(lane0.parent / "somebody-elses-directory")
        # ...and one into an idle lane's tree: resolved, it would be a legitimate target, which is why a symlink
        # in the plan is refused outright rather than followed
        lane_link = lane0.parent / "alias-of-lane1-target"
        lane_link.symlink_to(lane1 / "rust-modules/target", target_is_directory=True)
        # the seed and a live FFmpeg cache entry
        seed = self.make_tree(cache / "cargo-seed/target")
        entries = [
            ("evict", 4, "tree", 1, "x", 0, "normal", lane0 / "pkg"),
            ("evict", 4, "tree", 1, "x", 0, "normal", lane0 / "vendor/ffmpeg-build"),
            ("evict", 4, "tree", 1, "x", 0, "normal", tracked),
            ("evict", 0, "tree", 1, "-", 1440, "normal", foreign),
            ("evict", 4, "tree", 1, "x", 0, "normal", link),
            ("evict", 4, "tree", 1, "x", 0, "normal", lane_link),
            ("evict", 0, "tree", 1, "x", 0, "normal", repo / "rust-modules/target"),
            ("evict", 4, "tree", 1, "x", 0, "normal", lane1 / "rust-modules/target/debug/incremental"),
            ("evict", 4, "tree", 1, "x", 0, "normal", elsewhere),
            ("evict", 0, "tree", 1, "-", 0, "normal", seed),
            ("evict", 0, "tree", 1, "-", 0, "normal", cache / "ffmpeg/synthetic-key"),
            ("evict", 4, "tree", 1, "main", 0, "normal", repo / "rust-modules/target"),
            ("evict", 2, "incremental", 1, "main", 0, "normal", repo / "rust-modules/target/debug/incremental"),
            ("evict", 6, "tree", 1, "main", 0, "normal", repo / "rust-modules/target"),
            ("evict", 6, "incremental", 1, "lane1", 0, "normal", lane1 / "rust-modules/target/debug/incremental"),
            ("evict", 4, "tree", 1, "x", 0, "normal", "rust-modules/target"),
            ("evict", 4, "tree", 1, "x", 0, "normal", lane1 / "rust-modules/target/debug"),
            ("evict", 4, "incremental", 1, "x", 0, "normal", lane1 / "rust-modules/target"),
            ("evict", 9, "tree", 1, "x", 0, "normal", lane1 / "rust-modules/target"),
            ("evict", 4, "everything", 1, "x", 0, "normal", lane1 / "rust-modules/target"),
            ("evict", 4, "tree", 1, "x", 0, "elsewhere", lane1 / "rust-modules/target"),
            ("delete", 4, "tree", 1, "x", 0, "normal", lane1 / "rust-modules/target"),
        ]
        before = {p: self.snapshot_tree(p) for p in (repo, lane0, lane1, fleet, cache, tmproot, elsewhere)}
        for entry in entries:
            with self.subTest(path=str(entry[-1])[-48:], rung=entry[1], scope=entry[2], mode=entry[6], kind=entry[0]):
                plan = self.write_plan(fixture, [entry])
                result = self.run_evict(fixture, plan)
                out = result.stdout + result.stderr
                self.assertEqual(result.returncode, 1, out)
                self.assertIn("REFUSED", out)
        after = {p: self.snapshot_tree(p) for p in before}
        self.assertEqual(before, after, "a refused path was touched")
        self.assertTrue(link.is_symlink())

    def test_evict_refuses_a_plan_in_a_format_it_was_not_written_for(self):
        fixture, (lane0, _) = self.evict_fixture()
        for header in ("# plx-disk-plan v2", "", "evict"):
            with self.subTest(header=header):
                plan = self.write_plan(fixture, [("evict", 4, "tree", 1, "x", 0, "normal", lane0 / "rust-modules/target")],
                                       header=header)
                result = self.run_evict(fixture, plan)
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertTrue((lane0 / "rust-modules/target/sentinel").exists())

    def test_evict_needs_a_plan_that_exists_and_a_planner_to_ask(self):
        fixture, (lane0, _) = self.evict_fixture()
        bare = self.run_gc(fixture, "--evict")
        self.assertEqual(bare.returncode, 2, bare.stdout + bare.stderr)
        self.assertIn("needs a plan file", bare.stderr)
        missing = self.run_evict(fixture, fixture[0].parent / "no-such-plan.tsv")
        self.assertEqual(missing.returncode, 2, missing.stdout + missing.stderr)
        plain, (plane0, _) = self.evict_fixture(planner=False)
        plan = self.write_plan(plain, [("evict", 4, "tree", 1, "x", 0, "normal", plane0 / "rust-modules/target")])
        result = self.run_evict(plain, plan)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("refusing", result.stderr)
        self.assertTrue((plane0 / "rust-modules/target/sentinel").exists(), "deleted without a planner to re-probe with")

    def test_evict_goes_on_while_an_ffmpeg_build_holds_the_cache_lock_and_leaves_its_entry(self):
        # Every other mode refuses for a held FFmpeg lock (assert_live_refusal). This one never touches the FFmpeg
        # cache's live entries, and an FFmpeg build is when the disk is likeliest to be short.
        fixture, (lane0, _) = self.evict_fixture(pid=os.getpid(), pgid=0)
        repo, env, sentinels, lock = fixture
        orphan = sentinels[1].parents[2]
        self.assertTrue(lock.exists())
        control = self.run_gc(fixture, "--orphans")
        self.assertNotEqual(control.returncode, 0, "the control mode should refuse under a held FFmpeg lock")
        self.assertIn("held FFmpeg cache lock", control.stdout + control.stderr)
        plan = self.write_plan(fixture, [("evict", 0, "tree", 64, "-", 0, "normal", orphan),
                                         ("evict", 4, "tree", 64, "lane0", 0, "normal", lane0 / "rust-modules/target")])
        result = self.run_evict(fixture, plan)
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertFalse(orphan.exists(), out)
        self.assertFalse((lane0 / "rust-modules/target").exists(), out)
        self.assertTrue(sentinels[2].exists(), "touched the FFmpeg cache's live entry: " + out)
        self.assertTrue(lock.exists())

    def test_evict_takes_a_cache_tombstone_but_never_the_seed_or_a_fresh_leftover(self):
        fixture, _ = self.evict_fixture()
        env = fixture[1]
        seeds = Path(env["PLX_BUILD_CACHE"]) / "cargo-seed"
        seed = self.make_tree(seeds / "target")
        tomb = self.make_tree(seeds / "target.tombstone.4242.1")
        fresh = self.make_tree(seeds / "target.tmp.4242")
        stale = self.make_tree(seeds / "target-release.restore.4242")
        self.age(stale, 3 * 86400)
        plan = self.write_plan(fixture, [("evict", 0, "tree", 8, "-", 0, "normal", tomb),
                                         ("evict", 0, "tree", 8, "-", 0, "normal", fresh),
                                         ("evict", 0, "tree", 8, "-", 0, "normal", stale),
                                         ])
        result = self.run_evict(fixture, plan)
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertFalse(tomb.exists(), out)
        self.assertTrue(fresh.exists(), "took an in-flight seed harvest: " + out)
        self.assertFalse(stale.exists(), "left a day-old seed leftover: " + out)
        self.assertTrue(seed.exists(), "took the seed: " + out)

    def test_evict_never_takes_a_tmp_tree_that_only_looks_old(self):
        # A test fixture's `utime` backdating moves the mtime, never the ctime: that tree is minutes old.
        fixture, _ = self.evict_fixture()
        env = fixture[1]
        tmp_tree = self.make_tree(Path(env["PLX_GC_TMP_ROOTS"]) / "cargo-target-fixture")
        (tmp_tree / "CACHEDIR.TAG").write_text("Signature: 8a477f597d28d172789f06886806bc55\n")
        deps = tmp_tree / "debug/deps"
        deps.mkdir(parents=True)
        (deps / "plxnative_modules-0123456789abcdef.d").write_text("ours\n")
        self.age(tmp_tree, 5 * 86400)
        plan = self.write_plan(fixture, [("evict", 0, "tree", 8, "-", 1440, "normal", tmp_tree)])
        young = self.run_evict(fixture, plan)
        out = young.stdout + young.stderr
        self.assertEqual(young.returncode, 0, out)
        self.assertIn("in use, skipped", out)
        self.assertRegex(out, r"changed .* ago")
        self.assertTrue((tmp_tree / "CACHEDIR.TAG").exists(), "evicted a /tmp tree written minutes ago: " + out)
        # The same fixture, with the clock really three days on, is old garbage.
        later = self.run_evict(fixture, plan, extra_env={"PLX_GC_TEST_NOW": str(time.time() + 3 * 86400)})
        out = later.stdout + later.stderr
        self.assertEqual(later.returncode, 0, out)
        self.assertFalse(tmp_tree.exists(), out)

    def test_evict_never_takes_a_tmp_tree_that_holds_a_registered_checkout(self):
        # A cargo tree under /tmp someone made a worktree inside. It is old, ours and unlocked, and git cannot
        # say it tracks a file (the repository is nested below it): only the checkout list keeps its source.
        fixture, (lane0, _) = self.evict_fixture()
        env = fixture[1]
        holder = self.make_tree(Path(env["PLX_GC_TMP_ROOTS"]) / "cargo-target-holder")
        (holder / "CACHEDIR.TAG").write_text("Signature: 8a477f597d28d172789f06886806bc55\n")
        (holder / "debug/deps").mkdir(parents=True)
        (holder / "debug/deps/plxnative_modules-0123456789abcdef.d").write_text("ours\n")
        nested = holder / "nested-lane"
        self.git(fixture, fixture[0], "worktree", "add", "-q", "-b", "nested-lane", str(nested))
        plan = self.write_plan(fixture, [("evict", 0, "tree", 8, "-", 1440, "normal", holder)])
        result = self.run_evict(fixture, plan, extra_env={"PLX_GC_TEST_NOW": str(time.time() + 3 * 86400)})
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 1, out)
        self.assertIn("REFUSED", out)
        self.assertTrue((nested / "README").exists(), "deleted a registered checkout's source: " + out)
        self.assertTrue((holder / "CACHEDIR.TAG").exists(), out)

    def test_evict_stops_once_free_space_is_back_over_the_plans_goal(self):
        fixture, (lane0, lane1) = self.evict_fixture()
        plan_lines = [("evict", 4, "tree", 96, "lane0", 0, "normal", lane0 / "rust-modules/target"),
                      ("evict", 4, "tree", 96, "lane1", 0, "normal", lane1 / "rust-modules/target")]
        enough = self.run_evict(fixture, self.write_plan(fixture, plan_lines, goal_kib=4 << 20),
                                extra_env={"PLX_GC_TEST_FREE_KIB": str(8 << 20)})
        out = enough.stdout + enough.stderr
        self.assertEqual(enough.returncode, 0, out)
        self.assertIn("back over the plan's goal", out)
        self.assertTrue((lane0 / "rust-modules/target").exists() and (lane1 / "rust-modules/target").exists(),
                        "deleted although free space was already over the goal: " + out)
        # Free space that rises as the run frees things: a `df` that reports 1 GiB until it has been asked twice.
        counter = fixture[0].parent / "df-calls"
        df = fixture[0].parent / "bin/df"
        df.write_text('#!/bin/sh\n'
                      'if [ "$1" = -Pk ]; then\n'
                      '  n=$(cat "%s" 2>/dev/null || echo 0); n=$((n + 1)); echo $n > "%s"\n'
                      '  [ $n -ge 2 ] && free=%d || free=%d\n'
                      '  printf "Filesystem 1024-blocks Used Available Capacity Mounted on\\nx 1 1 %%s 1%%%% /\\n" $free\n'
                      '  exit 0\n'
                      'fi\n'
                      'exec %s "$@"\n' % (counter, counter, 8 << 20, 1 << 20, shlex.quote(shutil.which("df"))))
        df.chmod(0o755)
        mid = self.run_evict(fixture, self.write_plan(fixture, plan_lines, goal_kib=4 << 20))
        out = mid.stdout + mid.stderr
        self.assertEqual(mid.returncode, 0, out)
        self.assertFalse((lane0 / "rust-modules/target").exists(), "the first path should have gone: " + out)
        self.assertTrue((lane1 / "rust-modules/target").exists(), "kept going past the goal: " + out)

    def test_evict_ends_with_the_settle_verdict_on_the_free_space_it_re_read(self):
        budget = ["# need_kib %d" % (4 << 20), "# reserve_kib %d" % (3 << 20)]
        for free_kib, code, word in ((9 << 20, 0, "ok"), (2 << 20, 3, "REFUSE")):
            with self.subTest(word=word):
                fixture, (lane0, _) = self.evict_fixture()
                line = ("evict", 4, "tree", 96, "lane0", 0, "normal", lane0 / "rust-modules/target")
                plan = self.write_plan(fixture, [line])
                plan.write_text(plan.read_text().replace("# verdict reclaim\n", "# verdict reclaim\n" + "\n".join(budget) + "\n"))
                result = self.run_evict(fixture, plan, extra_env={"PLX_GC_TEST_FREE_KIB": str(free_kib)})
                out = result.stdout + result.stderr
                self.assertEqual(result.returncode, code, out)
                self.assertIn("settle: " + word, out)

    def test_evict_can_take_the_current_checkouts_incremental_dir_only_in_mode_current(self):
        fixture, (lane0, lane1) = self.evict_fixture()
        for name in ("build-gc.sh", "disk-budget.py", "cargo-seed.py", "check-lock.py"):
            (lane0 / "tools").mkdir(exist_ok=True)
            shutil.copy(ROOT / "tools" / name, lane0 / "tools" / name)
        incr = lane0 / "rust-modules/target/debug/incremental"
        plan = self.write_plan(fixture, [("evict", 6, "incremental", 8, "lane0", 0, "normal", incr)])
        normal = self.run_evict(fixture, plan, cwd=lane0)
        self.assertEqual(normal.returncode, 1, normal.stdout + normal.stderr)
        self.assertTrue(incr.exists())
        plan = self.write_plan(fixture, [("evict", 6, "incremental", 8, "lane0", 0, "current", incr),
                                         ("evict", 6, "tree", 8, "lane0", 0, "current", lane0 / "rust-modules/target")])
        current = self.run_evict(fixture, plan, cwd=lane0)
        out = current.stdout + current.stderr
        self.assertEqual(current.returncode, 1, out)      # the whole tree is refused
        self.assertFalse(incr.exists(), out)
        self.assertTrue((lane0 / "rust-modules/target/sentinel").exists(), "took more than its incremental dir: " + out)

    def test_evict_run_from_a_lane_never_takes_that_lanes_own_tree_at_rungs_0_2_4(self):
        # The checkout `--evict` runs from is the one about to build (the build-entry preflight runs there), so
        # it gives up ONLY a rung-6 incremental dir in mode current. A plan naming its own tree at the rungs
        # other lanes give up is never written for a build, and taking it would be pure waste; a `--no-build`
        # plan made by hand from a lane can name it, so the line is SPARED (skipped, exit 0), not refused. The
        # same line run from the main checkout is taken.
        fixture, (lane0, lane1) = self.evict_fixture()
        for name in ("build-gc.sh", "disk-budget.py", "cargo-seed.py", "check-lock.py"):
            (lane0 / "tools").mkdir(exist_ok=True)
            shutil.copy(ROOT / "tools" / name, lane0 / "tools" / name)
        own = lane0 / "rust-modules/target"
        # ...and a lane that keeps its CARGO_TARGET_DIR in the fleet dir is the same checkout
        fleet_own = self.make_tree(Path(fixture[1]["PLX_FLEET_DIR"]) / "lane0/target", incremental=True)
        for tree in (own, fleet_own):
            for rung, scope, sub in ((0, "tree", ""), (2, "incremental", "debug/incremental"), (4, "tree", "")):
                for mode in ("normal", "current"):
                    with self.subTest(tree=tree.parent.name, rung=rung, scope=scope, mode=mode):
                        plan = self.write_plan(fixture, [("evict", rung, scope, 8, "lane0", 0, mode, tree / sub)])
                        result = self.run_evict(fixture, plan, cwd=lane0)
                        out = result.stdout + result.stderr
                        self.assertEqual(result.returncode, 0, out)
                        self.assertIn("spared: the checkout --evict runs from", out)
                        self.assertTrue((tree / "sentinel").exists(), "took the running checkout's own tree: " + out)
                        self.assertTrue((tree / "debug/incremental/work-product").exists(), out)
            with self.subTest(tree=tree.parent.name, rung=6, mode="normal"):
                plan = self.write_plan(fixture, [("evict", 6, "incremental", 8, "lane0", 0, "normal",
                                                  tree / "debug/incremental")])
                result = self.run_evict(fixture, plan, cwd=lane0)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)    # refused outright
                self.assertTrue((tree / "debug/incremental/work-product").exists())
        plan = self.write_plan(fixture, [("evict", 6, "incremental", 8, "lane0", 0, "current",
                                          fleet_own / "debug/incremental")])
        taken = self.run_evict(fixture, plan, cwd=lane0)
        self.assertEqual(taken.returncode, 0, taken.stdout + taken.stderr)
        self.assertFalse((fleet_own / "debug/incremental").exists(), taken.stdout + taken.stderr)
        self.assertTrue((fleet_own / "sentinel").exists(), "took more than its incremental dir")
        plan = self.write_plan(fixture, [("evict", 4, "tree", 8, "lane0", 0, "normal", own)])
        elsewhere = self.run_evict(fixture, plan)         # the same line, run from the main checkout
        self.assertEqual(elsewhere.returncode, 0, elsewhere.stdout + elsewhere.stderr)
        self.assertFalse(own.exists(), elsewhere.stdout + elsewhere.stderr)

    def test_evict_dry_run_prints_the_plan_and_changes_nothing_and_logs_nothing(self):
        fixture, (lane0, lane1) = self.evict_fixture()
        repo, env, sentinels, _ = fixture
        orphan = sentinels[1].parents[2]
        plan = self.write_plan(fixture, [("evict", 0, "tree", 64, "-", 0, "normal", orphan),
                                         ("evict", 4, "tree", 64, "lane1", 0, "normal", lane1 / "rust-modules/target")],
                               goal_kib=1 << 30)
        root = repo.parent

        def state():
            return {str(p.relative_to(root)): (p.stat().st_mtime_ns, p.read_bytes() if p.is_file() else None)
                    for p in root.rglob("*") if p != plan}
        before = state()
        result = self.run_evict(fixture, plan, "-n")
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertEqual(out.count("would remove"), 2, out)
        self.assertEqual(before, state())
        self.assertFalse(Path(env["PLX_GC_LOG"]).exists(), "a dry run wrote the log")

    def test_evict_refuses_when_worktrees_cannot_be_enumerated(self):
        fixture, (lane0, _) = self.evict_fixture()
        fake_git = fixture[0].parent / "bin/git"
        fake_git.write_text("#!/bin/sh\nexit 1\n")
        fake_git.chmod(0o755)
        plan = self.write_plan(fixture, [("evict", 4, "tree", 1, "x", 0, "normal", lane0 / "rust-modules/target")])
        result = self.run_evict(fixture, plan)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot enumerate", result.stderr)
        self.assertTrue((lane0 / "rust-modules/target/sentinel").exists())

    def test_evict_runs_the_stale_sweep_at_its_slot_and_not_at_any_other_rung(self):
        fixture, _ = self.evict_fixture()
        repo = fixture[0]
        deps = repo / "rust-modules/target/debug/deps"
        deps.mkdir(parents=True)
        old = time.time() - 30 * 3600
        superseded = [deps / "plxnative_modules-aaaaaaaaaaaaaaaa", deps / "plxnative_modules-aaaaaaaaaaaaaaaa.d"]
        newest = [deps / "plxnative_modules-bbbbbbbbbbbbbbbb", deps / "plxnative_modules-bbbbbbbbbbbbbbbb.d"]
        for p in superseded:
            p.write_bytes(b"")
            os.utime(p, (old, old))
        for p in newest:
            p.write_bytes(b"")
        misplaced = self.run_evict(fixture, self.write_plan(
            fixture, [("stale", 3, "stale", 0, "-", 0, "normal", repo / "rust-modules/target")]))
        self.assertEqual(misplaced.returncode, 1, misplaced.stdout + misplaced.stderr)
        self.assertTrue(all(p.exists() for p in superseded))
        result = self.run_evict(fixture, self.write_plan(
            fixture, [("stale", 5, "stale", 0, "-", 0, "normal", repo / "rust-modules/target")]))
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertIn("the --stale sweep", out)
        self.assertTrue(all(not p.exists() for p in superseded), "the sweep did not run: " + out)
        self.assertTrue(all(p.exists() for p in newest), "the sweep took the newest hash: " + out)


class InstallDiskWatchTests(unittest.TestCase):
    """Regression: running the installer FROM a linked worktree must still resolve and launch
    the MAIN checkout's own `tools/build-gc.sh`, never the worktree's — that worktree can be
    removed by a squash-merge teardown while the plist it would otherwise have baked in outlives
    it. Only the resolution is under test; launchd/tmutil are stubbed out entirely."""

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="install-disk-watch-tests-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()

    def test_resolves_the_main_checkout_not_the_calling_lane(self):
        main = self.root / "main"
        (main / "tools").mkdir(parents=True)
        for name in ("install-disk-watch.sh", "build-gc.sh"):
            (main / "tools" / name).write_bytes((ROOT / "tools" / name).read_bytes())
        (main / "tools" / "install-disk-watch.sh").chmod(0o755)

        home = self.root / "home"
        (home / "Library/LaunchAgents").mkdir(parents=True)
        (home / "Library/Logs").mkdir(parents=True)
        stub_bin = self.root / "bin"
        stub_bin.mkdir()
        # `uname` -> Darwin so the script takes the launchd path; `launchctl` is a silent no-op
        # (this test is about WHICH plist gets written, not about a real gui/$(id -u) domain);
        # `tmutil` fails `destinationinfo` so the Time Machine block is skipped without needing a
        # real backup destination on the runner.
        (stub_bin / "uname").write_text("#!/bin/sh\necho Darwin\n")
        (stub_bin / "launchctl").write_text("#!/bin/sh\nexit 0\n")
        (stub_bin / "tmutil").write_text("#!/bin/sh\nexit 1\n")
        for name in ("uname", "launchctl", "tmutil"):
            (stub_bin / name).chmod(0o755)

        env = dict(os.environ)
        env["HOME"] = str(home)
        env["PATH"] = str(stub_bin) + os.pathsep + env.get("PATH", "")

        subprocess.run(["git", "init", "-q", str(main)], env=env, check=True)
        (main / "README").write_text("seed\n")
        subprocess.run(["git", "-C", str(main), "add", "README", "tools"], env=env, check=True)
        subprocess.run(["git", "-C", str(main), "-c", "user.email=t@t", "-c", "user.name=t",
                        "commit", "-q", "-m", "seed"], env=env, check=True)
        subprocess.run(["git", "-C", str(main), "branch", "-m", "main"], env=env, check=True)
        lane = self.root / "lane0"
        subprocess.run(["git", "-C", str(main), "worktree", "add", "-q", "-b", "lane0",
                        str(lane)], env=env, check=True)

        # The installer is invoked FROM the lane, not from `main` — exactly the case that broke:
        # `ROOT=$(cd "$(dirname "$0")/.." && pwd)` alone would resolve to the LANE.
        result = run_guarded(["sh", str(lane / "tools" / "install-disk-watch.sh")], lane, env)
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        plist = home / "Library/LaunchAgents/com.plxnative.build-gc.plist"
        self.assertTrue(plist.exists(), diagnostic)
        contents = plist.read_text()
        self.assertIn(str(main / "tools/build-gc.sh"), contents,
                      "plist did not point at the main checkout's script:\n" + contents)
        self.assertNotIn(str(lane), contents,
                         "plist baked in the calling lane's own path:\n" + contents)


class MakeCheckContractTests(unittest.TestCase):
    def test_host_check_runs_gc_regressions(self):
        # `make check` is `tools/check-lock.py`'s machine-wide queue wrapper (two slots) around
        # `check-unlocked`, which fans out to `check-python` and so to the step manifest
        # (ci/check-python-steps.txt) this asserts on; `make check` still runs it, just queued.
        lines = (ROOT / "Makefile").read_text().splitlines()

        def recipe_of(target):
            start = next(i for i, line in enumerate(lines) if line.startswith(target + ":"))
            recipe = []
            for line in lines[start + 1:]:
                if line and not line.startswith(("\t", "#")):
                    break
                recipe.append(line)
            return recipe

        # `check-unlocked` runs the `check-cargo` and `check-python` branches side by side; the
        # Python-only gates (this one) live in the second.
        unlocked = "\n".join(recipe_of("check-unlocked"))
        self.assertIn("check-cargo", unlocked)
        self.assertIn("check-python", unlocked)
        self.assertIn("python3 ci/test_build_gc.py", check_steps.commands())


if __name__ == "__main__":
    unittest.main()
