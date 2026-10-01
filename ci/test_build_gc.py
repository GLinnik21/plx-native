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
MODES = ("--incremental", "--orphans", "--lanes", "--stale", "--cache", "--worktrees", "--all")


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
                   GIT_CONFIG_NOSYSTEM="1",
                   # `--auto`'s log and lock live under THIS fixture's own root — never the real
                   # ~/Library/Logs or /tmp/plx-build-gc-auto.lock, which a concurrent real
                   # `--auto` (the SessionEnd hook, launchd) could be holding on the very machine
                   # running this suite.
                   PLX_GC_LOG=str(root / "gc.log"), PLX_GC_LOCK_DIR=str(root / "gc.lock"))
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

    def run_auto(self, fixture, *args, extra_env=None, timeout=30):
        repo, env, _, _ = fixture
        env = dict(env)
        if extra_env:
            env.update(extra_env)
        return subprocess.run(["sh", "tools/build-gc.sh", "--auto", *args], cwd=repo, env=env,
                              text=True, capture_output=True, timeout=timeout)

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
        result = subprocess.run(["sh", "-x", "tools/build-gc.sh", "--worktrees"], cwd=repo,
                                env=env, text=True, capture_output=True, timeout=20)
        trace = result.stdout + result.stderr
        name = roots[0].name
        self.assertEqual(result.returncode, 0, trace)
        self.assertNotIn(f"ext=/{name}", trace,
                         "built a root-anchored external path from an empty PLX_FLEET_DIR: "
                         + trace)
        self.assertFalse(roots[0].exists(), "worktree was not actually removed: " + trace)

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
        result = subprocess.run(["sh", "tools/build-gc.sh", "--stale"], cwd=repo, env=env,
                                text=True, capture_output=True, timeout=20)
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

    def test_auto_below_threshold_runs_every_stage_in_order(self):
        # An impossible threshold (999999 GiB) against a tiny stub free value keeps every stage
        # gated "below threshold" throughout, including after stages that free nothing (this repo
        # never actually reclaims enough to cross a threshold that high) — the point is to prove
        # the STAGE ORDER, not a real crossing.
        fixture = self.fixture(0, 0)
        roots = self._add_worktrees(fixture, 2, prefix="lane", add_target=True)
        result = self.run_auto(fixture, extra_env={
            "PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999",
            "PLX_GC_IDLE_MIN": "0"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        for wt in roots:
            self.assertFalse((wt / "rust-modules/target").exists(),
                             "idle lane target survived a below-threshold --auto: " + diagnostic)
        repo, env, _, _ = fixture
        log = Path(env["PLX_GC_LOG"]).read_text()
        order = [s for s in ("orphans:", "incremental:", "worktrees:", "lanes")
                 if s in log]
        self.assertEqual(order, ["orphans:", "incremental:", "worktrees:", "lanes"],
                         "stages ran out of order:\n" + log)

    def test_auto_idle_guard_spares_a_recently_touched_lane(self):
        # Same setup as the ordering test, but the DEFAULT idle window (60 minutes) — a lane
        # `_add_worktrees` just created has a target dir with an mtime of right now, so it must be
        # spared even though free space is far below threshold. A stopped agent might resume in
        # the next few minutes; a needless rebuild is real money, not a rounding error.
        fixture = self.fixture(0, 0)
        roots = self._add_worktrees(fixture, 1, prefix="lane", add_target=True)
        result = self.run_auto(fixture, extra_env={
            "PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("recently active, skipped", diagnostic)
        self.assertTrue((roots[0] / "rust-modules/target").exists(),
                        "a lane touched seconds ago was reclaimed: " + diagnostic)

    def test_auto_idle_guard_checks_the_external_tree_too(self):
        # A lane's `cargo build` writes into `$PLX_FLEET_DIR/<lane>/target`, not into the
        # worktree — `fleet-plan` exports `CARGO_TARGET_DIR` precisely so `git worktree remove`
        # stays meaningful. So a worktree that has sat untouched past the idle window, while its
        # lane is still mid-build (the only fresh mtime is in the external tree), must still be
        # spared. Checking only the worktree's own mtime — the bug this test guards — would
        # reclaim a tree a live build is using out from under it.
        #
        # `add_target=False`, and dirty by an untracked file OUTSIDE `rust-modules/target*`: a
        # `rust-modules/target` inside the worktree would itself get deleted by the earlier
        # "derived trees in linked worktrees" stage, and that deletion bumps the worktree's own
        # directory mtime to "now" — which would make `idle_lane "$_w"` read active anyway and
        # mask exactly the bug this test exists to catch. The scratch file keeps `git status
        # --porcelain` non-empty (so the even-earlier `--worktrees` stage does not remove the
        # whole worktree, and the external tree along with it) without giving any stage before
        # `--lanes` something of its own to delete.
        fixture = self.fixture(0, 0)
        roots = self._add_worktrees(fixture, 1, prefix="lane", add_target=False)
        wt = roots[0]
        (wt / "scratch.txt").write_text("keeps git status dirty\n")
        old = time.time() - 3600
        for p in [wt, *wt.rglob("*")]:
            try:
                os.utime(p, (old, old), follow_symlinks=False)
            except (FileNotFoundError, NotADirectoryError, OSError):
                pass
        repo, env, sentinels, _ = fixture
        fleet = Path(env["PLX_FLEET_DIR"])
        ext_target = fleet / wt.name / "target"
        ext_target.mkdir(parents=True)
        (ext_target / "sentinel").write_text("synthetic build output\n")
        result = self.run_auto(fixture, extra_env={
            "PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999",
            "PLX_GC_IDLE_MIN": "30"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("recently active, skipped", diagnostic)
        self.assertTrue((ext_target / "sentinel").exists(),
                        "external tree with a fresh write was reclaimed: " + diagnostic)

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
        fixture = self.fixture(0, 0)
        roots = self._add_worktrees(fixture, 1, prefix="lane", add_target=True)
        result = self.run_auto(fixture, "-n", extra_env={
            "PLX_GC_TEST_FREE_KIB": "1", "PLX_GC_MIN_FREE_GIB": "999999",
            "PLX_GC_IDLE_MIN": "0"})
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        self.assertIn("would remove", diagnostic)
        self.assertTrue((roots[0] / "rust-modules/target").exists(),
                        "-n deleted a lane target: " + diagnostic)
        _, env, sentinels, _ = fixture
        for s in sentinels:
            self.assertTrue(s.exists(), "-n deleted a sentinel: " + diagnostic)
        self.assertFalse(Path(env["PLX_GC_LOCK_DIR"]).exists(),
                         "-n took the single-instance lock: " + diagnostic)


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
        result = subprocess.run(["sh", str(lane / "tools" / "install-disk-watch.sh")],
                                cwd=lane, env=env, text=True, capture_output=True, timeout=20)
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
        # `make check` is `tools/check-lock.py`'s machine-wide queue wrapper around
        # `check-unlocked`, which fans out to the `check-python` recipe this asserts on;
        # `make check` still runs it, just serialized.
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
        self.assertIn("\tpython3 ci/test_build_gc.py", recipe_of("check-python"))


if __name__ == "__main__":
    unittest.main()
