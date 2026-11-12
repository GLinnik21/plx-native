#!/usr/bin/env python3
"""Tests for tools/cold-store.py, the opt-in cold store that parks idle build trees on a second volume.

Everything runs on temp directories. The two volumes are faked (`device_of` says the `das/` directory is
another device, `free_bytes_of` reports a settable free space), `ditto` is replaced by a hard-link-preserving
`shutil` walk so the suite runs where there is no `ditto`, and one darwin-only test at the end drives the real
`ditto`. The properties pinned here are the ones whose failure is silent: an unconfigured machine growing files,
a park/unpark that changes a path, size, nanosecond mtime or hard-link count, an unpark that overwrites a tree a
build already made, a crash leaving a tree both unparked and deleted, and a pretend-emergency test environment
parking the real trees.
"""
import contextlib
import errno
import importlib.util
import io
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import types
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(os.path.dirname(HERE), "tools", "cold-store.py")

spec = importlib.util.spec_from_file_location("cold_store", SCRIPT)
cold = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cold)

REAL_FREE_BYTES = cold.free_bytes_of                       # Rig replaces it; this test wants the real one
REAL_FLUSH = getattr(cold, "flush_cold_volume", None)       # and this one: Rig stubs the flush (a sync costs real time)
GIB = 1 << 30
BASE_NS = 1_700_000_000_000_000_000


def copy_walk(src, dst):
    """The test stand-in for `ditto`: modes, ns mtimes, symlinks and hard links preserved."""
    seen = {}
    dirs = []
    os.mkdir(dst)
    for dirpath, dirnames, filenames in os.walk(src):
        rel = os.path.relpath(dirpath, src)
        target = dst if rel == "." else os.path.join(dst, rel)
        dirs.append((dirpath, target))
        for name in list(dirnames):
            path = os.path.join(dirpath, name)
            if os.path.islink(path):
                dirnames.remove(name)
                filenames.append(name)
            else:
                os.mkdir(os.path.join(target, name))
        for name in filenames:
            path, out = os.path.join(dirpath, name), os.path.join(target, name)
            st = os.lstat(path)
            if os.path.islink(path):
                os.symlink(os.readlink(path), out)
                os.utime(out, ns=(st.st_atime_ns, st.st_mtime_ns), follow_symlinks=False)
            elif st.st_nlink > 1 and (st.st_dev, st.st_ino) in seen:
                os.link(seen[(st.st_dev, st.st_ino)], out)
            else:
                shutil.copy2(path, out)
                if st.st_nlink > 1:
                    seen[(st.st_dev, st.st_ino)] = out
    for src_dir, out_dir in reversed(dirs):
        shutil.copystat(src_dir, out_dir)


def make_tree(path, salt=0):
    """A cargo-looking tree: a hard link between deps/ and incremental/, a symlink, an empty file, odd mtimes."""
    files = {"debug/deps/libfoo.rlib": "rlib%d" % salt * 700, "debug/build/x/out.txt": "out %d" % salt,
             "debug/.fingerprint/foo/invoked.timestamp": "", ".rustc_info.json": "{}"}
    for rel, text in files.items():
        full = os.path.join(path, rel)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w") as fh:
            fh.write(text)
    os.makedirs(os.path.join(path, "debug/incremental/foo-1"))
    os.link(os.path.join(path, "debug/deps/libfoo.rlib"), os.path.join(path, "debug/incremental/foo-1/dep-graph.bin"))
    os.symlink("deps", os.path.join(path, "debug/latest"))
    n = 0
    for dirpath, dirnames, filenames in os.walk(path, topdown=False):
        for name in filenames + [d for d in dirnames if os.path.islink(os.path.join(dirpath, d))]:
            n += 1
            os.utime(os.path.join(dirpath, name), ns=(BASE_NS + n * 1_000_003, BASE_NS + n * 1_000_003 + 17),
                     follow_symlinks=False)
        n += 1
        os.utime(dirpath, ns=(BASE_NS + n * 999_983, BASE_NS + n * 999_983 + 5))


def snap(root):
    """Paths, sizes, ns mtimes, modes, link targets, hard-link counts and hard-link groups of a tree."""
    entries, groups = {}, {}
    for dirpath, dirnames, filenames in os.walk(root):
        for name in dirnames + filenames:
            path = os.path.join(dirpath, name)
            st = os.lstat(path)
            rel = os.path.relpath(path, root)
            kind = "l" if os.path.islink(path) else ("d" if os.path.isdir(path) else "f")
            entries[rel] = (kind, st.st_size if kind != "d" else 0, st.st_mtime_ns, st.st_mode, st.st_nlink,
                            os.readlink(path) if kind == "l" else None)
            if kind == "f" and st.st_nlink > 1:
                groups.setdefault((st.st_dev, st.st_ino), set()).add(rel)
    st = os.lstat(root)
    entries["."] = ("d", 0, st.st_mtime_ns, st.st_mode, 0, None)
    return entries, sorted(sorted(g) for g in groups.values())


def walk_all(root):
    return sorted(os.path.join(d, n) for d, ds, fs in os.walk(root) for n in ds + fs)


def dead_pid():
    proc = subprocess.Popen([sys.executable, "-c", "pass"])
    proc.wait()
    return proc.pid


class Rig(unittest.TestCase):
    """A fake two-volume machine: `das/` is the cold volume (device 2), everything else is device 1."""

    def setUp(self):
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix="plx-cold-test-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.volume = os.path.join(self.tmp, "das")
        self.root = os.path.join(self.volume, "cold")
        self.cache = os.path.join(self.tmp, "cache")
        self.checkout = os.path.join(self.tmp, "checkout")
        os.makedirs(self.volume)
        os.makedirs(os.path.join(self.checkout, "rust-modules"))
        self.config = os.path.join(self.tmp, "cold-store.cfg")
        self.write_config("root=%s\n" % self.root)
        self.env = {"PLX_BUILD_CACHE": self.cache, "PLX_COLD_STORE_CONFIG": self.config,
                    "PLX_TV_CONFIG_DIR": os.path.join(self.tmp, "no-such-dir")}
        self.free = 1000 * GIB
        self.logs = []
        self.flushes = []
        for target, fake in (("device_of", self.fake_dev), ("which_ditto", lambda: "/usr/bin/ditto"),
                             ("copy_tree", copy_walk), ("free_bytes_of", lambda path, env: self.free),
                             ("flush_cold_volume", self.flushes.append)):
            patcher = mock.patch.object(cold, target, fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    def fake_dev(self, path):
        return 2 if os.path.realpath(path).startswith(self.volume) else 1

    def write_config(self, text):
        with open(self.config, "w") as fh:
            fh.write(text)

    def store(self, env=None):
        return cold.ColdStore(self.env if env is None else env, log=self.logs.append)

    def tree(self, name="target", salt=0, lane="lane"):
        path = os.path.join(self.tmp, lane, "rust-modules", name)
        make_tree(path, salt)
        return path

    def park(self, store, path, **kw):
        kw.setdefault("kind", "lane")
        kw.setdefault("checkout", os.path.dirname(os.path.dirname(path)))
        return store.park(path, **kw)


class Unconfigured(Rig):
    def entry_points(self, store):
        self.assertEqual(store.available(1), store.off_reason)
        tree = os.path.join(self.tmp, "t")
        os.makedirs(tree)
        for out in (store.park(tree, kind="lane"), store.unpark(tree)):
            self.assertEqual(out.status, cold.SKIPPED)
        self.assertEqual((store.list(), store.recover(), store.prune(), store.parking_residue(), store.indexed(tree)),
                         ([], [], [], [], False))

    def test_no_config_creates_nothing_anywhere(self):
        os.unlink(self.config)
        before = walk_all(self.tmp)
        store = self.store()
        self.assertFalse(store.configured())
        self.entry_points(store)
        self.assertEqual(walk_all(self.tmp), sorted(before + [os.path.join(self.tmp, "t")]))
        self.assertFalse(os.path.exists(self.cache))

    def test_switched_off_by_env_even_with_a_config(self):
        store = self.store(dict(self.env, PLX_COLD_STORE="off"))
        self.assertIn("PLX_COLD_STORE=off", store.off_reason)
        self.entry_points(store)
        self.assertFalse(os.path.exists(self.cache))
        self.assertFalse(os.path.exists(self.root))

    def test_a_relative_root_is_not_configured(self):
        self.write_config("root=relative/dir\n")
        self.assertIn("absolute", self.store().off_reason)

    def test_cli_status_says_so_and_writes_nothing(self):
        os.unlink(self.config)
        out = io.StringIO()
        with mock.patch.dict(os.environ, self.env), contextlib.redirect_stdout(out):
            self.assertEqual(cold.main(["status"]), 0)
            self.assertEqual(cold.main(["prune"]), 0)
        self.assertIn("not configured", out.getvalue())
        self.assertFalse(os.path.exists(self.cache))


class Config(Rig):
    def test_defaults_and_overrides(self):
        cfg = self.store().cfg
        self.assertEqual((cfg.root, cfg.max_days, cfg.min_free_gib, cfg.park_below_gib, cfg.park_idle_min,
                          cfg.park_max_gib_per_run, cfg.rate_floor_mibs), (self.root, 14, 200, 40, 120, 20, 40))
        self.assertEqual([cfg.rebuild_secs(d) for d in ("target", "target-fast", "target-release", "target-storage")],
                         [300, 120, 600, 30])
        self.assertEqual(cfg.rebuild_secs("some-tmp-tree"), cold.FALLBACK_REBUILD_SECS)
        self.write_config("# comment\nroot = %s\nmax_days=3\nmin_free_gib=oops\nrebuild_secs_target=7\n"
                          "rebuild_secs_weird-dir=9\n" % self.root)
        cfg = self.store().cfg
        self.assertEqual((cfg.max_days, cfg.min_free_gib, cfg.rebuild_secs("target"), cfg.rebuild_secs("weird-dir")),
                         (3, 200, 7, 9))

    def test_env_root_overrides_the_file(self):
        self.assertEqual(self.store(dict(self.env, PLX_COLD_STORE="/tmp/elsewhere")).cfg.root, "/tmp/elsewhere")

    def test_the_config_dir_follows_tv_config_sh(self):
        self.assertEqual(cold.config_dir({"PLX_TV_CONFIG_DIR": "/a"}), "/a")
        self.assertEqual(cold.config_dir({"XDG_CONFIG_HOME": "/x"}), "/x/plxnative")
        self.assertEqual(cold.config_dir({"PLX_TV_CONFIG_DIR": "", "XDG_CONFIG_HOME": ""}),
                         os.path.join(os.path.expanduser("~"), ".config", "plxnative"))
        env = {"PLX_TV_CONFIG_DIR": os.path.join(self.tmp, "cfgdir")}
        os.makedirs(env["PLX_TV_CONFIG_DIR"])
        with open(os.path.join(env["PLX_TV_CONFIG_DIR"], "cold-store"), "w") as fh:
            fh.write("root=%s\n" % self.root)
        self.assertEqual(self.store(env).cfg.root, self.root)

    def test_escape_is_reversible_and_fits_a_file_name(self):
        self.assertEqual(cold.escape("/a/b%c"), "%2Fa%2Fb%25c")
        self.assertNotEqual(cold.escape("/a/b"), cold.escape("/a%2Fb"))
        long = "/" + "/".join(["very-long-directory-name"] * 12) + "/target"
        self.assertLessEqual(len(cold.escape(long)), 255)
        self.assertNotEqual(cold.escape(long), cold.escape(long + "x"))


class Available(Rig):
    def test_reasons_in_order(self):
        store = self.store()
        with mock.patch.object(cold, "which_ditto", lambda: None):
            self.assertIn("ditto", store.available(1))
        gone = self.store(dict(self.env, PLX_COLD_STORE="/Volumes/PlxColdStoreTestNoSuchVolume/x"))
        self.assertIn("not mounted", gone.available(1))
        self.assertFalse(os.path.exists("/Volumes/PlxColdStoreTestNoSuchVolume"))
        self.assertIn("same volume", store.available(2))
        self.free = 150 * GIB
        self.assertIn("min_free_gib=200", store.available(1))
        self.free = 1000 * GIB
        self.assertFalse(os.path.exists(self.root))          # nothing written while a check failed
        self.assertIsNone(store.available(1))
        self.assertTrue(os.path.isdir(self.root))             # the probe made the root, and left no probe file
        self.assertEqual(os.listdir(self.root), [])

    def test_a_failing_probe_write_is_a_reason(self):
        blocker = os.path.join(self.volume, "file")
        open(blocker, "w").close()
        store = self.store(dict(self.env, PLX_COLD_STORE=os.path.join(blocker, "sub")))
        self.assertIn("cannot write", store.available(1))


class RoundTrip(Rig):
    def test_park_then_unpark_restores_everything(self):
        path = self.tree()
        before = snap(path)
        self.assertTrue(before[1])                            # the fixture really has a hard link
        store = self.store()
        out = self.park(store, path, head="abc123", private_bytes=4242, now=1000.0)
        self.assertEqual(out.status, cold.PARKED, out)
        self.assertFalse(os.path.lexists(path))
        ident = cold.escape(path)
        self.assertEqual(os.listdir(os.path.join(self.cache, "cold-store", "parking")), [])
        self.assertTrue(store.indexed(path))
        rec = store.records()[0]
        self.assertEqual((rec["origin"], rec["kind"], rec["head"], rec["parked_at"], rec["private_bytes_freed"]),
                         (path, "lane", "abc123", 1000.0, 4242))
        self.assertEqual((rec["files"], rec["apparent_bytes"]), cold.tree_stats(os.path.join(self.root, "trees", ident)))
        self.assertGreater(rec["park_rate"], 0)
        self.assertEqual(cold.read_json(os.path.join(self.root, "trees", ident + ".json")), rec)
        out = store.unpark(path, rebuild_secs=1e9)
        self.assertEqual(out.status, cold.UNPARKED, out)
        self.assertEqual(snap(path), before)                  # paths, sizes, ns mtimes, modes, nlink, link groups
        self.assertFalse(store.indexed(path))
        self.assertEqual(sorted(os.listdir(os.path.dirname(path))), ["target"])    # no .unpark sibling left
        self.assertTrue(re.fullmatch(r"cold-store: unpark [\d.]+ [KMG]?i?B in [\d.]+ s: %s" % re.escape(path),
                                     self.logs[-1]), self.logs)
        left = os.listdir(os.path.join(self.root, "trees"))
        self.assertEqual(len(left), 1, left)
        self.assertRegex(left[0], r"^%s\.consumed\.\d+\.\d+$" % re.escape(ident))
        self.assertEqual(store.recover(), [("reaped", os.path.join(self.root, "trees", left[0]))])
        self.assertEqual(os.listdir(os.path.join(self.root, "trees")), [])

    def test_parking_again_replaces_the_older_cold_copy(self):
        store = self.store()
        path = self.tree()
        self.assertEqual(self.park(store, path, now=1.0).status, cold.PARKED)
        make_tree(path, salt=9)
        newer = snap(path)
        self.assertEqual(self.park(store, path, now=2.0).status, cold.PARKED)
        self.assertEqual(sorted(os.listdir(os.path.join(self.root, "trees"))), [cold.escape(path), cold.escape(path) + ".json"])
        self.assertEqual(store.unpark(path, rebuild_secs=1e9).status, cold.UNPARKED)
        self.assertEqual(snap(path), newer)

    def test_unpark_says_what_it_is_about_to_copy_before_it_copies(self):
        # An unpark runs inside a `make`: one line before a multi-minute copy, one after it.
        store = self.store()
        path = self.tree()
        self.assertEqual(self.park(store, path).status, cold.PARKED)
        seen = []

        def spy(src, dst):
            seen.append(list(self.logs))
            copy_walk(src, dst)
        with mock.patch.object(cold, "copy_tree", spy):
            out = store.unpark(path, rebuild_secs=1e9)
        self.assertEqual(out.status, cold.UNPARKED, out)
        self.assertEqual(len(seen), 1)
        before = r"cold-store: unpark [\d.]+ [KMG]?i?B from %s \(est [\d.]+ s at [\d.]+ MiB/s\): %s" % (
            re.escape(self.root), re.escape(path))
        self.assertEqual(len(seen[0]), 1, seen[0])
        self.assertRegex(seen[0][0], before)
        self.assertEqual(len(self.logs), 2, self.logs)
        self.assertRegex(self.logs[1], r"cold-store: unpark [\d.]+ [KMG]?i?B in [\d.]+ s: %s" % re.escape(path))

    def test_park_unpark_park_unpark_of_the_same_origin_leaks_nothing(self):
        # The second unpark's `.consumed` set-aside met the first one's, which nobody had reaped: the rename failed
        # quietly, the stale `trees/<id>` stayed (its record already gone) and the next park choked on it.
        store = self.store()
        path = self.tree()
        ident = cold.escape(path)
        trees = os.path.join(self.root, "trees")
        for cycle in range(2):
            before = snap(path)
            self.assertEqual(self.park(store, path, now=1.0 + cycle).status, cold.PARKED, self.logs)
            out = store.unpark(path, rebuild_secs=1e9)
            self.assertEqual(out.status, cold.UNPARKED, out)
            self.assertEqual(snap(path), before)
        left = sorted(os.listdir(trees))
        self.assertEqual(len(left), 2, left)
        for name in left:
            self.assertRegex(name, r"^%s\.consumed\.\d+\.\d+$" % re.escape(ident))
        self.assertEqual({what for what, _ in store.recover()}, {"reaped"})
        self.assertEqual(os.listdir(trees), [])

    def test_a_legacy_consumed_name_and_a_path_that_merely_contains_the_word_are_told_apart(self):
        store = self.store()
        trees = os.path.join(self.root, "trees")
        os.makedirs(os.path.join(trees, "old.consumed"))                      # what an earlier version left
        keeper = self.tree(name="target", lane="x.consumed.1.2")              # a lane directory with the word in it
        self.assertEqual(self.park(store, keeper).status, cold.PARKED)
        got = store.recover()
        self.assertEqual(got, [("reaped", os.path.join(trees, "old.consumed"))])
        self.assertEqual(store.unpark(keeper, rebuild_secs=1e9).status, cold.UNPARKED)

    def test_an_unrecorded_stale_copy_in_the_way_is_set_aside_by_the_next_park(self):
        store = self.store()
        path = self.tree()
        ident = cold.escape(path)
        stale = os.path.join(self.root, "trees", ident)
        os.makedirs(os.path.join(stale, "stale-dir"))                         # no record: a failed set-aside left it
        with open(os.path.join(stale, "stale-file"), "w") as fh:
            fh.write("old\n")
        before = snap(path)
        self.assertEqual(self.park(store, path).status, cold.PARKED, self.logs)
        self.assertEqual(store.unpark(path, rebuild_secs=1e9).status, cold.UNPARKED)
        self.assertEqual(snap(path), before, "the stale copy was restored instead of the parked tree")
        self.assertEqual({what for what, _ in store.recover()}, {"reaped"})
        self.assertEqual(os.listdir(os.path.join(self.root, "trees")), [])

    def test_a_dead_set_aside_name_that_is_taken_does_not_stop_a_park(self):
        store = self.store()
        path = self.tree()
        self.assertEqual(self.park(store, path, now=1.0).status, cold.PARKED)
        make_tree(path, salt=9)
        ident = cold.escape(path)
        os.makedirs(os.path.join(self.root, "trees", "%s.dead.%d" % (ident, os.getpid()), "x"))   # an old name, in use
        self.assertEqual(self.park(store, path, now=2.0).status, cold.PARKED, self.logs)

    def test_unpark_refuses_an_existing_origin(self):
        store = self.store()
        path = self.tree()
        self.assertEqual(self.park(store, path).status, cold.PARKED)
        make_tree(path, salt=5)                               # a build made a new one meanwhile
        mine = snap(path)
        with mock.patch.object(cold, "copy_tree", side_effect=AssertionError("copied onto an existing tree")):
            out = store.unpark(path, rebuild_secs=1e9)
        self.assertEqual(out.status, cold.REFUSED)
        self.assertEqual(snap(path), mine)
        self.assertTrue(store.indexed(path))
        self.assertEqual(sorted(os.listdir(os.path.dirname(path))), ["target"])

    def test_a_tree_made_while_unpark_copies_wins(self):
        store = self.store()
        path = self.tree()
        self.assertEqual(self.park(store, path).status, cold.PARKED)

        def copy_while_a_build_starts(src, dst):
            copy_walk(src, dst)
            make_tree(path, salt=7)
        with mock.patch.object(cold, "copy_tree", copy_while_a_build_starts):
            out = store.unpark(path, rebuild_secs=1e9)
        self.assertEqual(out.status, cold.REFUSED)
        mine = snap(path)
        make_tree(os.path.join(self.tmp, "ref"), salt=7)
        self.assertEqual(sorted(mine[0]), sorted(snap(os.path.join(self.tmp, "ref"))[0]))
        self.assertEqual(sorted(os.listdir(os.path.dirname(path))), ["target"])
        self.assertTrue(store.indexed(path))

    def test_unpark_declines_when_the_cost_rule_fails(self):
        store = self.store()
        path = self.tree()
        self.assertEqual(self.park(store, path).status, cold.PARKED)
        out = store.unpark(path, rebuild_secs=1e-9)
        self.assertEqual(out.status, cold.DECLINED, out)
        self.assertFalse(os.path.lexists(path))
        self.assertTrue(store.indexed(path))
        self.assertEqual(os.listdir(os.path.dirname(path)) if os.path.isdir(os.path.dirname(path)) else [], [])
        out = store.unpark(path, rebuild_secs=1e9, free_after_ok=lambda size: False)
        self.assertEqual(out.status, cold.DECLINED)
        self.assertFalse(os.path.lexists(path))
        asked = []
        self.assertEqual(store.unpark(path, rebuild_secs=1e9, free_after_ok=lambda size: asked.append(size) or True).status,
                         cold.UNPARKED)
        self.assertEqual(asked, [cold.tree_stats(path)[1]])

    def test_the_cost_rule_uses_the_slower_of_the_measured_rate_and_the_floor(self):
        store = self.store()
        path = self.tree()
        self.assertEqual(self.park(store, path).status, cold.PARKED)
        rec = store.records()[0]
        rec["park_rate"] = 1.0                                # 1 B/s measured: slower than the 40 MiB/s floor
        cold.write_json(store.index_path(path), rec)
        self.assertEqual(cold.ColdStore.rate_for(rec, 40), 1.0)
        self.assertEqual(cold.ColdStore.rate_for(dict(rec, park_rate=1e9), 40), 40 * cold.MIB)
        self.assertEqual(cold.ColdStore.rate_for(dict(rec, park_rate=0.0), 40), 40 * cold.MIB)   # unmeasured: the floor
        self.assertEqual(store.unpark(path, rebuild_secs=1.0).status, cold.DECLINED)
        rec["park_rate"] = 1e9                                # fast measured: the floor still bounds the estimate
        cold.write_json(store.index_path(path), rec)
        self.assertEqual(store.unpark(path, rebuild_secs=1e9).status, cold.UNPARKED)

    def test_an_unpark_with_a_pruned_cold_copy_drops_its_record(self):
        store = self.store()
        path = self.tree()
        self.assertEqual(self.park(store, path).status, cold.PARKED)
        shutil.rmtree(os.path.join(self.root, "trees", cold.escape(path)))
        self.assertEqual(store.unpark(path, rebuild_secs=1e9).status, cold.SKIPPED)
        self.assertFalse(store.indexed(path))

    def test_nothing_parked_is_a_skip_and_a_stat(self):
        path = os.path.join(self.tmp, "x", "target")
        os.makedirs(os.path.dirname(path))
        self.assertEqual(self.store().unpark(path).status, cold.SKIPPED)
        self.assertFalse(os.path.exists(self.cache))


class Claims(Rig):
    """An unpark CLAIMS its parked copy under the caller's lock and copies WITHOUT it (docs/disk-budget.md, "The cold
    store"): the claim is a rename of the SSD index record to `index/<id>.json.unparking.<pid>`, which hides the
    entry from `records()` / `indexed()` (a second unparker sees nothing to take), keeps `recover` from adopting it
    again and keeps the park pass off the origin, and whose pid says whether the claimer is still there."""

    def parked(self, name="target", lane="lane", salt=0):
        store = self.store()
        path = self.tree(name, salt=salt, lane=lane)
        self.assertEqual(self.park(store, path).status, cold.PARKED)
        return store, path

    def claim_files(self):
        index = os.path.join(self.cache, "cold-store", "index")
        return sorted(n for n in os.listdir(index) if ".unparking." in n)

    def foreign_claim(self, store, path, pid):
        """Another process's claim, as it leaves it: the entry renamed to its pid's name."""
        os.rename(store.index_path(path), "%s.unparking.%d" % (store.index_path(path), pid))

    def live_pid(self):
        proc = subprocess.Popen(["sleep", "60"])
        self.addCleanup(proc.wait)
        self.addCleanup(proc.terminate)
        return proc.pid

    def test_a_claim_takes_the_entry_out_of_the_index_and_unpark_finishes_it(self):
        store, path = self.parked()
        out = store.claim(path, rebuild_secs=1e9)
        self.assertEqual((out.status, (out.record or {}).get("origin")), (cold.CLAIMED, path), out)
        self.assertEqual(self.claim_files(), [cold.escape(path) + ".json.unparking.%d" % os.getpid()])
        self.assertFalse(store.indexed(path), "a claimed entry is still offered to a second unparker")
        self.assertEqual(store.records(), [])
        self.assertFalse(os.path.lexists(path), "a claim copies nothing")
        again = store.claim(path, rebuild_secs=1e9)
        self.assertEqual(again.status, cold.SKIPPED, again)
        self.assertIn("being restored by pid %d" % os.getpid(), again.reason)
        done = store.unpark(path, claimed=out.record)
        self.assertEqual(done.status, cold.UNPARKED, done)
        self.assertEqual(self.claim_files(), [])
        self.assertEqual(store.records(), [])
        self.assertTrue(os.path.isdir(path))
        self.assertFalse(os.path.exists(os.path.join(self.root, "trees", cold.escape(path))), "the cold copy was kept")

    def test_a_tree_another_live_process_has_claimed_is_named_and_copies_nothing(self):
        store, path = self.parked()
        pid = self.live_pid()
        self.foreign_claim(store, path, pid)
        with mock.patch.object(cold, "copy_tree", side_effect=AssertionError("copied a tree somebody else restores")):
            for call in (lambda: store.claim(path, rebuild_secs=1e9), lambda: store.unpark(path, rebuild_secs=1e9)):
                out = call()
                self.assertEqual(out.status, cold.SKIPPED, out)
                self.assertIn("being restored by pid %d" % pid, out.reason)
        self.assertEqual(self.claim_files(), [cold.escape(path) + ".json.unparking.%d" % pid])
        self.assertEqual(store.claimed_by(path), pid)

    def test_a_dead_claimers_entry_is_put_back_by_recover_and_by_the_next_claim(self):
        store, path = self.parked()
        self.foreign_claim(store, path, dead_pid())
        self.assertEqual(store.claimed_by(path), 0, "a dead process is not a claimer")
        self.assertEqual(store.recover(), [("restored-unpark-claim", path)])
        self.assertTrue(store.indexed(path))
        self.assertEqual(self.claim_files(), [])
        self.foreign_claim(store, path, dead_pid())
        out = store.claim(path, rebuild_secs=1e9)             # the next build puts it right itself and takes it
        self.assertEqual(out.status, cold.CLAIMED, out)
        self.assertEqual(self.claim_files(), [cold.escape(path) + ".json.unparking.%d" % os.getpid()])
        self.assertEqual(store.unpark(path, claimed=out.record).status, cold.UNPARKED)

    def test_a_failed_copy_after_the_claim_puts_the_entry_back(self):
        store, path = self.parked()
        out = store.claim(path, rebuild_secs=1e9)
        with mock.patch.object(cold, "copy_tree", side_effect=OSError("the cold volume went away")):
            done = store.unpark(path, claimed=out.record)
        self.assertEqual(done.status, cold.FAILED, done)
        self.assertTrue(store.indexed(path), "the entry was lost with the failed copy")
        self.assertEqual(self.claim_files(), [])
        self.assertFalse(os.path.lexists(path))
        self.assertEqual([n for n in os.listdir(os.path.dirname(path)) if "unpark" in n], [])
        self.assertEqual(store.unpark(path, rebuild_secs=1e9).status, cold.UNPARKED)      # and it still restores later

    def test_a_restored_claim_never_overwrites_a_newer_record(self):
        store, path = self.parked()
        out = store.claim(path, rebuild_secs=1e9)
        newer = dict(out.record, parked_at=out.record["parked_at"] + 99)
        cold.write_json(store.index_path(path), newer)       # a later park recorded the origin meanwhile
        with mock.patch.object(cold, "copy_tree", side_effect=OSError("boom")):
            self.assertEqual(store.unpark(path, claimed=out.record).status, cold.FAILED)
        self.assertEqual(store.records(), [newer])
        self.assertEqual(self.claim_files(), [])

    def test_a_claim_that_is_gone_is_not_copied_for(self):
        store, path = self.parked()
        out = store.claim(path, rebuild_secs=1e9)
        os.unlink(os.path.join(self.cache, "cold-store", "index", self.claim_files()[0]))
        with mock.patch.object(cold, "copy_tree", side_effect=AssertionError("copied without a claim")):
            self.assertEqual(store.unpark(path, claimed=out.record).status, cold.SKIPPED)

    def test_a_declined_claim_touches_nothing(self):
        store, path = self.parked()
        for kw in ({"rebuild_secs": 1e-9}, {"rebuild_secs": 1e9, "free_after_ok": lambda size: False}):
            out = store.claim(path, **kw)
            self.assertEqual(out.status, cold.DECLINED, out)
            self.assertEqual(self.claim_files(), [])
            self.assertTrue(store.indexed(path))

    def test_recover_does_not_adopt_a_record_that_is_being_restored(self):
        store, path = self.parked()
        self.assertEqual(store.claim(path, rebuild_secs=1e9).status, cold.CLAIMED)
        self.assertEqual(store.recover(), [])
        self.assertFalse(store.indexed(path))
        self.assertEqual(len(self.claim_files()), 1)

    def test_the_park_pass_leaves_an_origin_that_is_being_restored_alone(self):
        store, path = self.parked()
        self.assertEqual(store.claim(path, rebuild_secs=1e9).status, cold.CLAIMED)
        make_tree(path, salt=4)                               # a build started its own tree meanwhile
        mine = snap(path)
        out = self.park(store, path)
        self.assertEqual(out.status, cold.REFUSED, out)
        self.assertIn("being restored by pid %d" % os.getpid(), out.reason)
        self.assertEqual(snap(path), mine)
        self.assertFalse(store.indexed(path))

    def test_the_free_space_rule_counts_what_other_live_claims_still_copy(self):
        # Without the caller's lock two restores no longer take turns, so the second one's "is there room left"
        # must see the bytes the first one is about to write.
        store = self.store()
        a, b = self.tree("target", lane="a"), self.tree("target-fast", lane="b", salt=1)
        for path in (a, b):
            self.assertEqual(self.park(store, path).status, cold.PARKED)
        size_a = store.claim(a, rebuild_secs=1e9).record["apparent_bytes"]
        size_b = cold.tree_stats(os.path.join(self.root, "trees", cold.escape(b)))[1]
        asked = []
        out = store.claim(b, rebuild_secs=1e9, free_after_ok=lambda n: asked.append(n) or True)
        self.assertEqual(out.status, cold.CLAIMED, out)
        self.assertEqual(asked, [size_a + size_b])
        self.assertEqual(store.inflight_bytes(1), size_a + size_b)

    def test_a_dead_claimers_bytes_are_not_in_flight(self):
        store = self.store()
        a, b = self.tree("target", lane="a"), self.tree("target-fast", lane="b", salt=1)
        for path in (a, b):
            self.assertEqual(self.park(store, path).status, cold.PARKED)
        self.foreign_claim(store, a, dead_pid())
        asked = []
        store.claim(b, rebuild_secs=1e9, free_after_ok=lambda n: asked.append(n) or True)
        self.assertEqual(asked, [cold.tree_stats(os.path.join(self.root, "trees", cold.escape(b)))[1]])


class ParkGuards(Rig):
    def test_a_live_tree_is_refused_and_untouched(self):
        path = self.tree()
        before = snap(path)
        asked = []
        out = self.park(self.store(), path, live=lambda p: asked.append(p) or "a cargo lock is held")
        self.assertEqual((out.status, asked), (cold.REFUSED, [path]))
        self.assertIn("cargo lock", out.reason)
        self.assertEqual(snap(path), before)
        self.assertFalse(os.path.exists(os.path.join(self.cache, "cold-store", "index")))

    def test_dry_run_changes_nothing(self):
        path = self.tree()
        before = snap(path)
        out = self.park(self.store(), path, dry_run=True)
        self.assertEqual(out.status, cold.DRY_RUN)
        self.assertEqual(snap(path), before)
        self.assertEqual(self.store().records(), [])

    def test_a_dry_run_creates_no_file_or_directory_on_either_volume(self):
        # `build-gc.sh --park -n` promises to change nothing: not the SSD's parking directory, not the cold root,
        # not a probe file on the cold volume (which a real park writes to prove the volume is writable).
        path = self.tree()
        shutil.rmtree(self.volume)
        os.makedirs(self.volume)                           # the cold volume is mounted but empty: no root yet
        before = walk_all(self.tmp)
        out = self.park(self.store(), path, dry_run=True)
        self.assertEqual(out.status, cold.DRY_RUN, out)
        self.assertEqual(walk_all(self.tmp), before)
        self.assertFalse(os.path.exists(self.cache))
        self.assertIsNone(self.store().available(1, write_probe=False))

    def test_refusals_leave_the_tree_alone(self):
        path = self.tree()
        before = snap(path)
        link = os.path.join(self.tmp, "link")
        os.symlink(path, link)
        self.assertEqual(self.park(self.store(), link).status, cold.REFUSED)
        self.assertEqual(self.park(self.store(), os.path.join(self.tmp, "absent")).status, cold.REFUSED)
        with mock.patch.object(cold, "device_of", lambda p: 2):            # the root shares the tree's volume
            self.assertIn("same volume", self.park(self.store(), path).reason)
        with mock.patch.object(cold, "device_of",
                               lambda p: 3 if os.path.realpath(p).startswith(self.cache) else self.fake_dev(p)):
            out = self.park(self.store(), path)
        self.assertEqual(out.status, cold.REFUSED)
        self.assertIn("another volume", out.reason)
        self.assertEqual(snap(path), before)

    def test_a_failed_copy_puts_the_tree_back(self):
        path = self.tree()
        before = snap(path)

        def boom(src, dst):
            os.mkdir(dst)
            raise OSError("disk went away")
        with mock.patch.object(cold, "copy_tree", boom):
            out = self.park(self.store(), path)
        self.assertEqual(out.status, cold.FAILED)
        self.assertEqual(snap(path), before)
        self.assertEqual(os.listdir(os.path.join(self.cache, "cold-store", "parking")), [])
        self.assertEqual(os.listdir(os.path.join(self.root, "trees")), [])

    def test_a_copy_that_does_not_verify_is_not_accepted(self):
        path = self.tree()
        before = snap(path)

        def lossy(src, dst):
            copy_walk(src, dst)
            os.unlink(os.path.join(dst, ".rustc_info.json"))
        with mock.patch.object(cold, "copy_tree", lossy):
            out = self.park(self.store(), path)
        self.assertEqual((out.status, snap(path)), (cold.FAILED, before))
        self.assertIn("verification", out.reason)
        self.assertEqual(self.store().records(), [])


class ParkDurability(Rig):
    """The copy is only as safe as the cold disk's write cache is flushed: nothing on the SSD may go before it is,
    and a step after the copy that fails must leave the tree where it was, not in parking/ with a traceback."""

    def trees(self):
        return sorted(os.listdir(os.path.join(self.root, "trees")))

    def parking(self):
        return os.listdir(os.path.join(self.cache, "cold-store", "parking"))

    @staticmethod
    def inject(name, when):
        """Make `cold.<name>` (or `os.rename`) raise an OSError for the calls `when(*args)` picks; every other call
        is the real one."""
        owner = os if name == "rename" else cold
        real = getattr(owner, name)

        def wrapped(*args, **kw):
            if when(*args):
                raise OSError(errno.EIO, "injected %s failure" % name)
            return real(*args, **kw)
        return mock.patch.object(owner, name, wrapped)

    def assertParkFailedAndTheTreeIsBack(self, store, path, before, out):
        ident = cold.escape(path)
        self.assertEqual(out.status, cold.FAILED, out)
        self.assertEqual(snap(path), before)
        self.assertEqual(self.parking(), [])
        self.assertEqual(store.records(), [])
        left = self.trees()
        self.assertNotIn(ident, left)
        self.assertNotIn(ident + ".json", left)
        self.assertNotIn(ident + ".partial", left)
        return left

    def test_the_cold_volume_is_flushed_after_the_copy_and_before_the_ssd_copy_goes(self):
        store = self.store()
        path = self.tree()
        ident = cold.escape(path)
        parking = os.path.join(self.cache, "cold-store", "parking", ident)
        seen = []

        def flush(record):
            seen.append({"record": record, "parking_copy": os.path.isdir(parking), "indexed": store.indexed(path),
                         "cold_copy": os.path.isdir(os.path.join(self.root, "trees", ident)),
                         "record_file": os.path.isfile(record)})
        with mock.patch.object(cold, "flush_cold_volume", flush):
            out = self.park(store, path)
        self.assertEqual(out.status, cold.PARKED, out)
        self.assertEqual(seen, [{"record": os.path.join(self.root, "trees", ident + ".json"), "parking_copy": True,
                                 "indexed": False, "cold_copy": True, "record_file": True}])
        self.assertTrue(store.indexed(path))
        self.assertFalse(os.path.lexists(parking))

    def test_a_flush_that_fails_puts_the_tree_back_and_leaves_no_cold_copy_for_recover_to_adopt(self):
        store = self.store()
        path = self.tree()
        before = snap(path)

        def boom(record):
            raise OSError(errno.EIO, "the drive did not flush")
        with mock.patch.object(cold, "flush_cold_volume", boom):
            out = self.park(store, path)
        left = self.assertParkFailedAndTheTreeIsBack(store, path, before, out)
        self.assertIn("did not flush", out.reason)
        self.assertTrue(left and all(".dead." in name for name in left), left)     # debris, not a copy
        got = store.recover()
        self.assertEqual({what for what, _ in got}, {"reaped"}, got)               # reaped, never adopted
        self.assertEqual((self.trees(), store.records(), snap(path)), ([], [], before))

    def test_a_failure_writing_the_cold_record_puts_the_tree_back(self):
        store = self.store()
        path = self.tree()
        before = snap(path)
        cold_json = os.path.join(self.root, "trees", cold.escape(path) + ".json")
        with self.inject("write_json", lambda target, obj: target == cold_json):
            out = self.park(store, path)
        self.assertParkFailedAndTheTreeIsBack(store, path, before, out)
        self.assertEqual(self.flushes, [], "nothing was complete to flush")

    def test_a_failure_renaming_the_copy_into_place_puts_the_tree_back(self):
        store = self.store()
        path = self.tree()
        before = snap(path)
        final = os.path.join(self.root, "trees", cold.escape(path))
        with self.inject("rename", lambda src, dst: dst == final):
            out = self.park(store, path)
        self.assertParkFailedAndTheTreeIsBack(store, path, before, out)
        self.assertEqual(self.trees(), [])

    def test_a_failure_moving_an_older_copy_aside_puts_the_tree_back(self):
        store = self.store()
        path = self.tree()
        self.assertEqual(self.park(store, path, now=1.0).status, cold.PARKED)
        make_tree(path, salt=9)
        before = snap(path)
        final = os.path.join(self.root, "trees", cold.escape(path))
        with self.inject("rename", lambda src, dst: src == final):
            out = self.park(store, path, now=2.0)
        self.assertEqual((out.status, snap(path), self.parking()), (cold.FAILED, before, []), out)

    def test_a_failure_writing_the_index_puts_the_tree_back_and_the_cold_copy_is_debris(self):
        store = self.store()
        path = self.tree()
        before = snap(path)
        with self.inject("write_json", lambda target, obj: target == store.index_path(path)):
            out = self.park(store, path)
        left = self.assertParkFailedAndTheTreeIsBack(store, path, before, out)
        self.assertTrue(left and all(".dead." in name for name in left), left)
        got = store.recover()
        self.assertEqual({what for what, _ in got}, {"reaped"}, got)
        self.assertEqual((self.trees(), store.records(), snap(path)), ([], [], before))

    def test_a_failed_undo_leaves_the_residue_for_recover_rather_than_deleting_the_tree(self):
        store = self.store()
        path = self.tree()
        before = snap(path)
        parked = os.path.join(self.cache, "cold-store", "parking", cold.escape(path))
        with self.inject("write_json", lambda target, obj: target == store.index_path(path)), \
                self.inject("rename", lambda src, dst: src == parked):
            out = self.park(store, path)
        self.assertEqual(out.status, cold.FAILED, out)
        self.assertTrue(os.path.isdir(parked))
        with open(parked + ".pid", "w") as fh:
            fh.write("%d\n" % dead_pid())                  # the parker exits after a FAILED; recover leaves a live one alone
        self.assertEqual([what for what, _ in store.recover() if what != "reaped"], ["renamed-back"])
        self.assertEqual(snap(path), before)


class FlushColdVolume(Rig):
    """The one function behind the flush: `sync`, then a full-drive flush through one file on the cold volume."""

    def run_flush(self, fake_fcntl):
        record = os.path.join(self.volume, "record.json")
        with open(record, "w") as fh:
            fh.write("{}\n")
        calls = []
        fake_fcntl.calls = calls
        inode = os.stat(record).st_ino

        def fsync(fd):
            calls.append(("fsync", os.fstat(fd).st_ino == inode))
        with mock.patch.object(cold, "fcntl", fake_fcntl), \
                mock.patch.object(cold.os, "sync", lambda: calls.append("sync")), \
                mock.patch.object(cold.os, "fsync", fsync):
            REAL_FLUSH(record)
        return calls, inode

    @staticmethod
    def fcntl_module(error=None, full=51):
        ns = types.SimpleNamespace()
        if full is not None:
            ns.F_FULLFSYNC = full

        def fcntl(fd, op):
            if error:
                raise error
            ns.calls.append(("full", op, os.fstat(fd).st_ino))
        ns.fcntl = fcntl
        return ns

    def test_it_syncs_then_flushes_the_whole_drive_through_the_record_file(self):
        calls, inode = self.run_flush(self.fcntl_module())
        self.assertEqual(calls, ["sync", ("full", 51, inode)])

    def test_it_falls_back_to_fsync_where_there_is_no_full_fsync(self):
        calls, _ = self.run_flush(self.fcntl_module(full=None))
        self.assertEqual(calls, ["sync", ("fsync", True)])

    def test_it_falls_back_to_fsync_on_a_volume_that_does_not_support_it(self):
        calls, _ = self.run_flush(self.fcntl_module(error=OSError(errno.ENOTSUP, "not supported")))
        self.assertEqual(calls, ["sync", ("fsync", True)])

    def test_any_other_failure_is_raised(self):
        with self.assertRaises(OSError):
            self.run_flush(self.fcntl_module(error=OSError(errno.EIO, "input/output error")))


class Recovery(Rig):
    """A parker that died (or was killed) mid-way: each state `recover` must settle without losing the tree."""

    def residue(self, path, owner=None, cold_copy=False, cold_parked_at=1.0, record=True):
        """Craft parking/<id>/ as a dead parker left it, from a real tree at `path` that is then removed."""
        store = self.store()
        ident = cold.escape(path)
        parked = os.path.join(self.cache, "cold-store", "parking", ident)
        os.makedirs(os.path.dirname(parked), exist_ok=True)
        files, size = cold.tree_stats(path)
        rec = {"id": ident, "origin": path, "kind": "lane", "checkout": "", "parked_at": 1.0, "files": files,
               "apparent_bytes": size}
        os.rename(path, parked)
        if record:
            cold.write_json(parked + ".json", rec)
        with open(parked + ".pid", "w") as fh:
            fh.write("%d\n" % (owner or dead_pid()))
        trees = os.path.join(self.root, "trees")
        os.makedirs(trees, exist_ok=True)
        os.mkdir(os.path.join(trees, ident + ".partial"))
        if cold_copy:
            copy_walk(parked, os.path.join(trees, ident))
            cold.write_json(os.path.join(trees, ident + ".json"), dict(rec, parked_at=cold_parked_at))
        return store, ident, parked

    def test_a_complete_cold_copy_deletes_the_ssd_copy(self):
        path = self.tree()
        store, ident, parked = self.residue(path, cold_copy=True)
        self.assertEqual(store.recover()[0], ("deleted-copy-done", ident))
        self.assertFalse(os.path.lexists(parked))
        self.assertFalse(os.path.lexists(path))
        self.assertTrue(store.indexed(path))                  # the lost index record was written back
        self.assertEqual(os.listdir(os.path.join(self.cache, "cold-store", "parking")), [])
        self.assertEqual(store.unpark(path, rebuild_secs=1e9).status, cold.UNPARKED)

    def test_an_absent_origin_gets_its_tree_renamed_back(self):
        path = self.tree()
        before = snap(path)
        store, ident, parked = self.residue(path)
        self.assertEqual(store.recover()[0], ("renamed-back", ident))
        self.assertEqual(snap(path), before)
        self.assertEqual(os.listdir(os.path.join(self.cache, "cold-store", "parking")), [])
        self.assertEqual(os.listdir(os.path.join(self.root, "trees")), [])      # the dead owner's .partial too

    def test_a_cold_copy_of_an_older_park_does_not_count_as_complete(self):
        path = self.tree()
        before = snap(path)
        store, ident, parked = self.residue(path, cold_copy=True, cold_parked_at=0.5)
        self.assertEqual(store.recover()[0], ("renamed-back", ident))
        self.assertEqual(snap(path), before)

    def test_a_present_origin_means_the_ssd_copy_is_stale(self):
        path = self.tree()
        store, ident, parked = self.residue(path)
        make_tree(path, salt=3)                               # a build recreated the tree
        mine = snap(path)
        self.assertEqual(store.recover()[0], ("deleted-stale", ident))
        self.assertFalse(os.path.lexists(parked))
        self.assertEqual(snap(path), mine)

    def test_a_live_owner_is_left_alone(self):
        path = self.tree()
        store, ident, parked = self.residue(path, owner=os.getpid())
        self.assertEqual(store.recover(), [])
        self.assertTrue(os.path.isdir(parked))
        self.assertEqual([r[0] for r in store.parking_residue()], [ident])
        self.assertTrue(store.parking_residue()[0][2])

    def test_a_tree_no_record_names_is_never_deleted(self):
        path = self.tree()
        store, ident, parked = self.residue(path, record=False)
        self.assertEqual(store.recover()[0], ("kept", ident))
        self.assertTrue(os.path.isdir(parked))

    def test_park_settles_a_dead_owners_residue_first(self):
        path = self.tree()
        store, ident, parked = self.residue(path)
        make_tree(path, salt=3)                               # the tree was rebuilt while the residue sat there
        out = self.park(store, path, now=5.0)                 # the residue is stale: deleted, then parked afresh
        self.assertEqual(out.status, cold.PARKED, out)
        self.assertEqual(store.recover(), [])

    def test_an_interrupted_unpark_sibling_is_removed(self):
        path = self.tree()
        store = self.store()
        self.assertEqual(self.park(store, path).status, cold.PARKED)
        sibling = os.path.join(os.path.dirname(path), ".target.unpark.%d" % dead_pid())
        os.makedirs(os.path.join(sibling, "debug"))
        live = os.path.join(os.path.dirname(path), ".target.unpark.%d" % os.getpid())
        os.makedirs(live)
        self.assertEqual(store.recover(), [("removed-unpark-residue", sibling)])
        self.assertTrue(os.path.isdir(live))

    def test_recover_adopts_cold_records_the_index_lost(self):
        store = self.store()
        path = self.tree()
        self.assertEqual(self.park(store, path).status, cold.PARKED)
        os.unlink(store.index_path(path))
        self.assertEqual(store.recover(), [("adopted", path)])
        self.assertTrue(store.indexed(path))


class Prune(Rig):
    def parked(self, store, lane, now, salt=0):
        path = self.tree(lane=lane, salt=salt)
        self.assertEqual(self.park(store, path, now=now, checkout=os.path.join(self.tmp, lane)).status, cold.PARKED)
        return path

    def pruned(self, store, **kw):
        return {os.path.basename(os.path.dirname(os.path.dirname(origin))): why for origin, why, _ in store.prune(**kw)}

    def test_each_rule(self):
        store = self.store()
        now = 100 * 86400.0
        rebuilt = self.parked(store, "rebuilt", now - 10)
        os.makedirs(rebuilt)
        os.utime(rebuilt, (now - 5, now - 5))                 # a tree made since the park
        older = self.parked(store, "older", now - 10)
        os.makedirs(older)
        os.utime(older, (now - 20, now - 20))                 # exists, but not built since: stays
        self.parked(store, "gone", now - 10)
        self.parked(store, "merged", now - 10)
        self.parked(store, "ancient", now - 15 * 86400)
        self.parked(store, "fresh", now - 10)
        facts = cold.PruneFacts(gone=[os.path.join(self.tmp, "gone")], merged=[os.path.join(self.tmp, "merged")])
        got = self.pruned(store, now=now, facts=facts, dry_run=True)
        self.assertEqual(len(store.records()), 6)             # a dry run drops nothing
        self.assertEqual(got, {"rebuilt": "the tree was rebuilt since it was parked", "gone": "its lane is gone",
                               "merged": "its lane is merged", "ancient": "older than max_days=14"})
        self.assertEqual(self.pruned(store, now=now, facts=facts), got)
        left = sorted(os.path.basename(os.path.dirname(os.path.dirname(r["origin"]))) for r in store.records())
        self.assertEqual(left, ["fresh", "older"])
        self.assertEqual(len(os.listdir(os.path.join(self.root, "trees"))), 4)    # 2 copies + 2 records

    def test_built_facts_override_the_directory_mtime(self):
        store = self.store()
        path = self.parked(store, "lane", 50.0)
        os.makedirs(path)
        os.utime(path, (10, 10))
        self.assertEqual(store.prune(now=60.0), [])
        got = store.prune(now=60.0, facts=cold.PruneFacts(built={path: 55.0}))
        self.assertEqual([g[1] for g in got], ["the tree was rebuilt since it was parked"])

    def test_the_oldest_go_while_the_cold_volume_is_under_min_free(self):
        store = self.store()
        for i, lane in enumerate(("a", "b", "c")):
            self.parked(store, lane, 1000.0 + i)

        def free(path, env):
            return 100 * GIB if len(store.records()) > 1 else 1000 * GIB
        with mock.patch.object(cold, "free_bytes_of", free):
            got = store.prune(now=2000.0)
        self.assertEqual([os.path.basename(os.path.dirname(os.path.dirname(g[0]))) for g in got], ["a", "b"])
        self.assertEqual([os.path.basename(os.path.dirname(os.path.dirname(r["origin"]))) for r in store.records()], ["c"])

    def test_prune_reaps_cold_side_debris_and_skips_an_unmounted_root(self):
        store = self.store()
        self.parked(store, "a", 1000.0)
        trees = os.path.join(self.root, "trees")
        for name in ("x.consumed", "y.dead.1"):
            os.makedirs(os.path.join(trees, name))
        os.makedirs(os.path.join(trees, "z.partial"))
        cold.write_json(os.path.join(trees, "orphan.json"), {"origin": "/nowhere"})
        store.prune(now=1001.0)
        self.assertEqual(sorted(os.listdir(trees)), [cold.escape(store.records()[0]["origin"]) + s for s in ("", ".json")])
        gone = self.store(dict(self.env, PLX_COLD_STORE="/Volumes/PlxColdStoreTestNoSuchVolume/x"))
        self.assertEqual(gone.prune(now=1e12), [])


class StopsForAWaiter(Rig):
    """`stop` is the park pass's "a preflight is waiting for the lock": recover, prune and the reap look at it
    before each piece of work and end there, so the lock goes back after one item, not after a whole sweep of
    the cold disk."""

    @staticmethod
    def after(n):
        """A stop callback that is False for `n` looks, then True."""
        calls = []
        return lambda: calls.append(1) or len(calls) > n

    def parking_entries(self):
        return sorted(n for n in os.listdir(os.path.join(self.cache, "cold-store", "parking"))
                      if not n.endswith((".json", ".pid")))

    def residues(self):
        for lane in ("l1", "l2", "l3"):
            Recovery.residue(self, self.tree(lane=lane))

    def parked_three(self, store, now):
        for i, lane in enumerate(("a", "b", "c")):
            Prune.parked(self, store, lane, now + i)

    def test_recover_settles_nothing_once_a_waiter_waits(self):
        self.residues()
        store = self.store()
        self.assertEqual(store.recover(stop=lambda: True), [])
        self.assertEqual(len(self.parking_entries()), 3)

    def test_recover_stops_between_residues(self):
        self.residues()
        store = self.store()
        got = store.recover(stop=self.after(1))
        self.assertEqual([what for what, _ in got], ["renamed-back"], got)
        self.assertEqual(len(self.parking_entries()), 2)
        self.assertEqual([what for what, _ in store.recover()], ["renamed-back"] * 2, "a later pass settles the rest")

    def test_recover_does_not_reap_once_a_waiter_waits(self):
        store = self.store()
        trees = os.path.join(self.root, "trees")
        os.makedirs(os.path.join(trees, "x.consumed"))
        self.assertEqual(store.recover(stop=lambda: True), [])
        self.assertTrue(os.path.isdir(os.path.join(trees, "x.consumed")))
        self.assertEqual(store.recover(), [("reaped", os.path.join(trees, "x.consumed"))])

    def test_prune_drops_nothing_once_a_waiter_waits(self):
        store = self.store()
        self.parked_three(store, 1.0)
        os.makedirs(os.path.join(self.root, "trees", "x.consumed"))
        self.assertEqual(store.prune(now=100 * 86400.0, stop=lambda: True), [])
        self.assertEqual(len(store.records()), 3)
        self.assertTrue(os.path.isdir(os.path.join(self.root, "trees", "x.consumed")), "no reap either")

    def test_prune_stops_between_copies_and_skips_the_reap(self):
        store = self.store()
        self.parked_three(store, 1.0)
        os.makedirs(os.path.join(self.root, "trees", "x.consumed"))
        got = store.prune(now=100 * 86400.0, stop=self.after(1))
        self.assertEqual(len(got), 1, got)
        self.assertEqual(len(store.records()), 2)
        self.assertTrue(os.path.isdir(os.path.join(self.root, "trees", "x.consumed")))
        self.assertEqual(len(store.prune(now=100 * 86400.0)), 2)
        self.assertFalse(os.path.isdir(os.path.join(self.root, "trees", "x.consumed")))

    def test_the_min_free_rule_stops_for_a_waiter_too(self):
        store = self.store()
        self.parked_three(store, 1000.0)
        with mock.patch.object(cold, "free_bytes_of", lambda path, env: 100 * GIB):       # always under min_free_gib
            got = store.prune(now=2000.0, stop=self.after(1))
        self.assertEqual(len(got), 1, got)
        self.assertEqual(len(store.records()), 2)


class TestEnvGuard(Rig):
    """A pretend free-space / clock override over the default roots must never reach a real tree."""

    def test_overrides_are_refused_unless_every_root_is_named(self):
        path = self.tree()
        before = snap(path)
        home = mock.patch.dict(os.environ, {"HOME": self.tmp})      # were the guard to fail, the "default" root is here
        home.start()
        self.addCleanup(home.stop)
        for name in cold.TEST_ENV:
            for drop in ("PLX_BUILD_CACHE", "PLX_COLD_STORE_CONFIG"):
                env = dict(self.env, **{name: "1000"})
                del env[drop]
                self.assertIn(drop, cold.refuse_test_env(env))
                store = self.store(env)
                if store.configured():
                    self.assertEqual(self.park(store, path).status, cold.REFUSED)
                    self.assertEqual(store.unpark(path).status, cold.REFUSED)
                    self.assertEqual(store.prune(now=1e12), [])
                    self.assertEqual(store.recover(), [])
                self.assertEqual(snap(path), before)
        self.assertFalse(os.path.exists(self.cache))
        self.assertFalse(os.path.exists(self.root))
        self.assertFalse(os.path.exists(os.path.join(self.tmp, ".cache")))

    def test_named_roots_make_the_overrides_honoured(self):
        env = dict(self.env, PLX_COLD_STORE_TEST_FREE_KIB=str(100 * GIB // 1024), PLX_COLD_STORE_TEST_NOW="123")
        self.assertEqual(cold.refuse_test_env(env), "")
        self.assertEqual(cold.clock(env), 123.0)
        self.assertEqual(REAL_FREE_BYTES(self.tmp, env), 100 * GIB)
        unnamed = {k: v for k, v in env.items() if k != "PLX_BUILD_CACHE"}
        self.assertNotEqual(cold.clock(unnamed), 123.0)
        self.assertNotEqual(REAL_FREE_BYTES(self.tmp, unnamed), 100 * GIB)


class Cli(Rig):
    def run_cli(self, *argv):
        out = io.StringIO()
        with mock.patch.dict(os.environ, self.env), contextlib.redirect_stdout(out):
            code = cold.main(list(argv))
        return code, out.getvalue()

    def test_status_list_park_unpark_prune_recover(self):
        path = self.tree()
        code, out = self.run_cli("status")
        self.assertEqual(code, 0)
        self.assertIn(self.root, out)
        self.assertIn("available:  yes", out)
        code, out = self.run_cli("park", path, "-n")
        self.assertEqual((code, os.path.isdir(path)), (0, True))
        self.assertIn("would park", out)
        with mock.patch.object(cold, "disk_budget_probe", lambda *a: (lambda p: None)):
            code, out = self.run_cli("park", path)
        self.assertEqual((code, os.path.lexists(path), out.strip()), (0, False, "parked"), out)
        code, out = self.run_cli("list")
        self.assertIn(path, out)
        self.assertIn("1 trees", self.run_cli("status")[1])
        self.assertEqual(self.run_cli("prune", "-n")[0], 0)
        code, out = self.run_cli("unpark", path, "-n")
        self.assertIn("would unpark", out)
        code, out = self.run_cli("unpark", path)
        self.assertEqual((code, os.path.isdir(path)), (0, True))
        self.assertEqual(self.run_cli("recover")[0], 0)


@unittest.skipUnless(sys.platform == "darwin" and shutil.which("ditto"), "needs macOS ditto")
class RealDitto(unittest.TestCase):
    def test_ditto_keeps_names_sizes_ns_mtimes_and_hard_links(self):
        tmp = os.path.realpath(tempfile.mkdtemp(prefix="plx-cold-ditto-"))
        self.addCleanup(shutil.rmtree, tmp, True)
        src, dst = os.path.join(tmp, "src"), os.path.join(tmp, "dst")
        make_tree(src)
        cold.copy_tree(src, dst)
        want, got = snap(src), snap(dst)
        self.assertEqual(sorted(want[0]), sorted(got[0]))
        self.assertEqual(want[1], got[1])
        for rel in want[0]:
            self.assertEqual(want[0][rel][:2] + want[0][rel][4:], got[0][rel][:2] + got[0][rel][4:], rel)
        # Measured 2026-11: ditto keeps the nanosecond mtime and the mode of every regular file and directory (the
        # inputs cargo's freshness check reads) but stamps a symlink with the copy time, which nothing reads.
        stale = [rel for rel in want[0] if want[0][rel][0] != "l" and want[0][rel][2:4] != got[0][rel][2:4]]
        self.assertEqual(stale, [], "ditto changed the nanosecond mtime or mode of these")


if __name__ == "__main__":
    unittest.main()
