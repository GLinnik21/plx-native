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
import importlib.util
import io
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(os.path.dirname(HERE), "tools", "cold-store.py")

spec = importlib.util.spec_from_file_location("cold_store", SCRIPT)
cold = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cold)

REAL_FREE_BYTES = cold.free_bytes_of                       # Rig replaces it; this test wants the real one
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
        for target, fake in (("device_of", self.fake_dev), ("which_ditto", lambda: "/usr/bin/ditto"),
                             ("copy_tree", copy_walk), ("free_bytes_of", lambda path, env: self.free)):
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
        self.assertEqual([n for n in os.listdir(os.path.join(self.root, "trees"))], [ident + ".consumed"])
        self.assertEqual(store.recover(), [("reaped", os.path.join(self.root, "trees", ident + ".consumed"))])

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
