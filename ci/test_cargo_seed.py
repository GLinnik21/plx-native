#!/usr/bin/env python3
"""Tests for tools/cargo-seed.py, the APFS-clone seed for a new lane's cargo target dirs.

Everything runs against a fake repository root and a private cache directory, with `clone_tree`
replaced by a plain copy so the suite also runs on CI runners whose filesystems cannot clone. The
real clonefile(2) path has one darwin-only test at the end. The properties pinned here are the
ones whose failure is silent: an app artifact leaking into a seed (a lane would link another
lane's code), a restore that merges into or overwrites a lane's target dir, a clone failure that
leaves a half tree, and the main checkout being seeded at all.
"""
import importlib.util
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(os.path.dirname(HERE), "tools", "cargo-seed.py")

spec = importlib.util.spec_from_file_location("cargo_seed", SCRIPT)
seed = importlib.util.module_from_spec(spec)
spec.loader.exec_module(seed)


def copy_clone(src, dst):
    shutil.copytree(src, dst, symlinks=True)


def touch(path, text="x", mtime=None):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as fh:
        fh.write(text)
    if mtime is not None:
        os.utime(path, (mtime, mtime))


def tree(path):
    out = set()
    for dirpath, _dirs, files in os.walk(path):
        for f in files:
            out.add(os.path.relpath(os.path.join(dirpath, f), path))
    return out


class SeedCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="plx-seed-test.")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.root = os.path.join(self.tmp, "lane")
        self.cache = os.path.join(self.tmp, "cache")
        os.makedirs(os.path.join(self.root, "rust-modules", "storage"))
        touch(os.path.join(self.root, ".git"), "gitdir: /elsewhere\n")  # a linked worktree
        touch(os.path.join(self.root, "rust-modules", "Cargo.lock"), "lock-v1\n")
        touch(os.path.join(self.root, "rust-modules", "Cargo.toml"), '[package]\nversion = "0.7.0"\nname = "a"\n')
        touch(os.path.join(self.root, "rust-modules", "storage", "Cargo.toml"), '[package]\nversion = "0.0.0"\n')
        self.bin = os.path.join(self.tmp, "bin")
        os.makedirs(self.bin)
        rustc = os.path.join(self.bin, "rustc")
        touch(rustc, '#!/bin/sh\necho "rustc 1.99.0 ${FAKE_RUSTC:-a} $*"\n')
        os.chmod(rustc, os.stat(rustc).st_mode | stat.S_IXUSR)
        self.env = {
            "PATH": self.bin + os.pathsep + os.environ.get("PATH", ""),
            "PLX_BUILD_CACHE": self.cache,
            "FAKE_RUSTC": "a",
            "CARGO_HOME": os.path.join(self.tmp, "cargo-home"),
        }
        saved = {k: os.environ.get(k) for k in list(self.env) + ["PLX_CARGO_SEED", "RUST_NIGHTLY"]}
        for k in ("PLX_CARGO_SEED", "RUST_NIGHTLY"):
            os.environ.pop(k, None)
        os.environ.update(self.env)
        self.addCleanup(self.restore_env, saved)
        self.real_clone = seed.clone_tree
        seed.clone_tree = copy_clone
        self.addCleanup(setattr, seed, "clone_tree", self.real_clone)
        self.tdir = os.path.join(self.root, "rust-modules", "target")
        # The script narrates what it did; a passing run should not.
        real_out, real_err = sys.stdout, sys.stderr
        sys.stdout = sys.stderr = open(os.devnull, "w")
        self.addCleanup(lambda: (sys.stdout.close(), setattr(sys, "stdout", real_out), setattr(sys, "stderr", real_err)))

    def restore_env(self, saved):
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def build_tdir(self, tdir=None):
        """A target dir shaped like a finished lane build: third-party and app output together."""
        t = tdir or self.tdir
        touch(os.path.join(t, "debug", "deps", "libserde-1111.rlib"), "serde", mtime=1_000_000)
        touch(os.path.join(t, "debug", "deps", "libserde-1111.rmeta"))
        touch(os.path.join(t, "debug", ".fingerprint", "serde-1111", "lib-serde"))
        touch(os.path.join(t, "debug", ".cargo-lock"), "")
        touch(os.path.join(t, "debug", "deps", "libplxnative_modules-9999.rlib"), "APP")
        touch(os.path.join(t, "debug", "deps", "plxnative_modules-9999"), "APP-TEST-BIN")
        touch(os.path.join(t, "debug", "deps", "plxnative_modules-9999.plxnative_modules.cgu.0.rcgu.o"), "APP")
        touch(os.path.join(t, "debug", ".fingerprint", "plxnative-modules-9999", "lib-plxnative_modules"))
        touch(os.path.join(t, "debug", "build", "plxnative-modules-9999", "output"), "APP")
        touch(os.path.join(t, "debug", "build", "serde-2222", "output"), "serde-build")
        touch(os.path.join(t, "debug", "plxnative-sim"), "APP-SIM")
        touch(os.path.join(t, "debug", "incremental", "x", "work"), "inc")
        touch(os.path.join(t, ".lib-artifacts.json"), "{}")
        return t

    def harvest(self, kind="target", tdir=None):
        seed.harvest(kind, tdir or self.tdir, self.root)

    def restore(self, kind="target", tdir=None):
        seed.restore(kind, tdir or self.tdir, self.root)

    def seed_dir(self, kind="target"):
        return os.path.join(self.cache, "cargo-seed", kind)

    def harvested(self, kind="target"):
        self.build_tdir()
        self.harvest(kind)
        self.assertTrue(os.path.isdir(self.seed_dir(kind)), "harvest created no seed")
        shutil.rmtree(self.tdir)


class Harvest(SeedCase):
    def test_the_app_crate_never_reaches_the_seed(self):
        self.build_tdir()
        self.harvest()
        files = tree(self.seed_dir())
        leaked = sorted(f for f in files if "plxnative" in f)
        self.assertEqual(leaked, [], "app artifacts in the seed")
        self.assertNotIn(".lib-artifacts.json", files)
        self.assertFalse(any("incremental" in f for f in files))
        self.assertIn(os.path.join("debug", "deps", "libserde-1111.rlib"), files)
        self.assertIn(os.path.join("debug", "build", "serde-2222", "output"), files)
        self.assertIn(seed.KEY_FILE, files)

    def test_harvest_leaves_the_lane_s_own_tree_alone(self):
        self.build_tdir()
        before = tree(self.tdir)
        self.harvest()
        self.assertEqual(tree(self.tdir), before)

    def test_a_fresh_seed_is_not_rewritten(self):
        self.build_tdir()
        self.harvest()
        marker = os.path.join(self.seed_dir(), "debug", "deps", "marker")
        touch(marker)
        self.harvest()
        self.assertTrue(os.path.exists(marker), "an up-to-date seed was replaced")

    def test_a_key_change_triggers_a_reharvest(self):
        self.build_tdir()
        self.harvest()
        stale = os.path.join(self.seed_dir(), "debug", "deps", "stale")
        touch(stale)
        os.environ["FAKE_RUSTC"] = "b"  # toolchain updated
        self.harvest()
        self.assertFalse(os.path.exists(stale), "a seed for the old toolchain survived")
        os.environ["FAKE_RUSTC"] = "a"
        touch(stale)
        touch(os.path.join(self.root, "rust-modules", "Cargo.lock"), "lock-v2\n")
        self.harvest()
        self.assertFalse(os.path.exists(stale), "a seed for the old Cargo.lock survived")
        self.assertEqual([n for n in os.listdir(os.path.join(self.cache, "cargo-seed")) if "." in n and n != "target.lock"], [])

    def test_a_second_lane_adds_the_directories_the_seed_lacks_and_evicts_nothing(self):
        # One lane ran the host suite (`debug/`), another did an ARM dev build (`arm-.../`) into the
        # same kind of tree; the seed must end up with both, not whichever came last.
        self.build_tdir()
        self.harvest()
        shutil.rmtree(self.tdir)
        touch(os.path.join(self.tdir, "arm-unknown-linux-gnueabi", "release", "deps", "libcore-3333.rlib"))
        touch(os.path.join(self.tdir, "arm-unknown-linux-gnueabi", "release", "deps", "libplxnative_modules-9999.a"))
        self.harvest()
        files = tree(self.seed_dir())
        self.assertIn(os.path.join("debug", "deps", "libserde-1111.rlib"), files)
        self.assertIn(os.path.join("arm-unknown-linux-gnueabi", "release", "deps", "libcore-3333.rlib"), files)
        self.assertFalse(any("plxnative" in f for f in files))
        self.assertEqual(sorted(os.listdir(os.path.join(self.cache, "cargo-seed"))), ["target", "target.lock"])

    def test_a_version_bump_does_not_change_the_key(self):
        before = seed.seed_key(self.root, "target")
        touch(os.path.join(self.root, "rust-modules", "Cargo.toml"), '[package]\nversion = "0.8.0"\nname = "a"\n')
        self.assertEqual(seed.seed_key(self.root, "target"), before)
        touch(os.path.join(self.root, "rust-modules", "Cargo.toml"), '[package]\nversion = "0.8.0"\nname = "a"\n[dependencies]\nx = "1"\n')
        self.assertNotEqual(seed.seed_key(self.root, "target"), before)

    def test_the_key_is_the_same_in_every_lane(self):
        # Two checkouts at different paths with the same toolchain, lock, manifests and configs must
        # agree, or a seed only ever matches the lane that made it (it did, once: the key hashed
        # each config file's PATH, and the first cross-lane restore silently built cold).
        other = os.path.join(self.tmp, "another", "lane")
        shutil.copytree(self.root, other)
        for r in (self.root, other):
            touch(os.path.join(r, "rust-modules", ".cargo", "config.toml"), "[unstable]\n")
        self.assertEqual(seed.seed_key(self.root, "target"), seed.seed_key(other, "target"))

    def test_a_seed_made_in_one_lane_is_restored_in_another(self):
        self.harvested()
        other = os.path.join(self.tmp, "another", "lane")
        shutil.copytree(self.root, other)
        seed.restore("target", os.path.join(other, "rust-modules", "target"), other)
        self.assertTrue(os.path.isdir(os.path.join(other, "rust-modules", "target", "debug")))

    def test_a_cargo_config_change_changes_the_key(self):
        before = seed.seed_key(self.root, "target")
        touch(os.path.join(self.root, "rust-modules", ".cargo", "config.toml"), "[build]\n")
        self.assertNotEqual(seed.seed_key(self.root, "target"), before)

    def test_a_build_in_progress_is_not_cloned(self):
        import fcntl
        self.build_tdir()
        fd = os.open(os.path.join(self.tdir, "debug", ".cargo-lock"), os.O_RDONLY)
        self.addCleanup(os.close, fd)
        fcntl.flock(fd, fcntl.LOCK_EX)
        self.harvest()
        self.assertFalse(os.path.exists(self.seed_dir()))

    def test_a_clone_failure_keeps_the_old_seed_and_leaves_no_partial_dir(self):
        self.build_tdir()
        self.harvest()
        good = tree(self.seed_dir())

        def broken(src, dst):
            os.makedirs(dst)
            touch(os.path.join(dst, "half"))
            raise OSError(45, "Operation not supported")

        seed.clone_tree = broken
        os.environ["FAKE_RUSTC"] = "c"
        self.harvest()
        self.assertEqual(tree(self.seed_dir()), good)
        self.assertEqual(sorted(os.listdir(os.path.join(self.cache, "cargo-seed"))), ["target", "target.lock"])


class Restore(SeedCase):
    def test_restore_creates_an_absent_dir_from_the_seed(self):
        self.harvested()
        self.assertFalse(os.path.exists(self.tdir))
        self.restore()
        files = tree(self.tdir)
        self.assertIn(os.path.join("debug", "deps", "libserde-1111.rlib"), files)
        self.assertFalse(any("plxnative" in f for f in files))
        self.assertNotIn(seed.KEY_FILE, files)
        self.assertNotIn(seed.USED_FILE, files)
        self.assertEqual(os.stat(os.path.join(self.tdir, "debug", "deps", "libserde-1111.rlib")).st_mtime, 1_000_000)

    def test_restore_refuses_an_existing_dir(self):
        self.harvested()
        os.makedirs(self.tdir)
        touch(os.path.join(self.tdir, "mine"), "mine")
        self.restore()
        self.assertEqual(tree(self.tdir), {"mine"})
        shutil.rmtree(self.tdir)
        os.makedirs(self.tdir)  # even an EMPTY one is a lane's own
        self.restore()
        self.assertEqual(tree(self.tdir), set())

    def test_restore_ignores_a_seed_with_another_key(self):
        self.harvested()
        os.environ["FAKE_RUSTC"] = "b"
        self.restore()
        self.assertFalse(os.path.exists(self.tdir))

    def test_restore_with_no_seed_is_a_no_op(self):
        self.restore()
        self.assertFalse(os.path.exists(self.tdir))

    def test_a_clone_failure_leaves_no_partial_dir(self):
        self.harvested()

        def broken(src, dst):
            os.makedirs(dst)
            touch(os.path.join(dst, "half"))
            raise seed.CloneUnsupported(0, "no clonefile here")

        seed.clone_tree = broken
        self.restore()
        self.assertFalse(os.path.lexists(self.tdir))
        self.assertEqual(sorted(os.listdir(os.path.join(self.cache, "cargo-seed"))), ["target", "target.lock"])

    def test_restore_is_never_a_full_copy_on_a_platform_that_cannot_clone(self):
        self.harvested()
        seed.clone_tree = self.real_clone
        if sys.platform == "darwin":
            self.skipTest("this platform clones")
        self.restore()
        self.assertFalse(os.path.lexists(self.tdir))


class Prune(SeedCase):
    def age(self, path, days):
        t = os.stat(path).st_mtime - days * 86400
        os.utime(path, (t, t))

    def test_prune_removes_only_seeds_unused_for_the_threshold(self):
        self.harvested("target")
        self.harvested("target-release")
        self.age(os.path.join(self.seed_dir("target"), seed.USED_FILE), 40)
        seed.prune(30, dry=False)
        self.assertFalse(os.path.exists(self.seed_dir("target")))
        self.assertTrue(os.path.isdir(self.seed_dir("target-release")))

    def test_a_dry_run_removes_nothing(self):
        self.harvested()
        self.age(os.path.join(self.seed_dir(), seed.USED_FILE), 40)
        seed.prune(30, dry=True)
        self.assertTrue(os.path.isdir(self.seed_dir()))

    def test_a_seed_being_restored_is_in_use_and_kept(self):
        import fcntl
        self.harvested()
        self.age(os.path.join(self.seed_dir(), seed.USED_FILE), 40)
        fd = os.open(os.path.join(self.cache, "cargo-seed", "target.lock"), os.O_RDONLY)
        self.addCleanup(os.close, fd)
        fcntl.flock(fd, fcntl.LOCK_SH)
        seed.prune(30, dry=False)
        self.assertTrue(os.path.isdir(self.seed_dir()))

    def test_interrupted_run_leftovers_are_swept_after_a_day(self):
        self.harvested()
        dead = os.path.join(self.cache, "cargo-seed", "target.tmp.99999")
        touch(os.path.join(dead, "half"))
        self.age(dead, 2)
        young = os.path.join(self.cache, "cargo-seed", "target.restore.99998")
        touch(os.path.join(young, "half"))
        seed.prune(30, dry=False)
        self.assertFalse(os.path.exists(dead))
        self.assertTrue(os.path.exists(young), "a leftover younger than a day may belong to a live run")
        self.assertTrue(os.path.isdir(self.seed_dir()))


class Gates(SeedCase):
    def test_off_disables_both_halves(self):
        os.environ["PLX_CARGO_SEED"] = "off"
        self.build_tdir()
        self.harvest()
        self.assertFalse(os.path.exists(self.seed_dir()))
        del os.environ["PLX_CARGO_SEED"]
        self.harvest()
        shutil.rmtree(self.tdir)
        os.environ["PLX_CARGO_SEED"] = "off"
        self.restore()
        self.assertFalse(os.path.exists(self.tdir))

    def test_the_main_checkout_is_never_seeded_or_harvested(self):
        self.harvested()  # a seed exists, made from a linked worktree
        git = os.path.join(self.root, ".git")
        os.unlink(git)
        os.makedirs(git)  # now a real .git directory: this is the main checkout
        self.restore()
        self.assertFalse(os.path.exists(self.tdir), "the main checkout was seeded")
        shutil.rmtree(self.seed_dir())
        self.build_tdir()
        self.harvest()
        self.assertFalse(os.path.exists(self.seed_dir()), "the main checkout was harvested")

    def test_only_the_named_kinds_are_seeded(self):
        self.build_tdir()
        self.harvest("target-lab")
        self.harvest("../escape")
        self.assertFalse(os.path.exists(os.path.join(self.cache, "cargo-seed")))

    def test_the_cli_never_fails_the_build(self):
        env = dict(os.environ)
        r = subprocess.run([sys.executable, SCRIPT, "harvest", "target", self.tdir, "--root", os.path.join(self.tmp, "nope")],
                           env=env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        r = subprocess.run([sys.executable, SCRIPT, "restore", "target-release", self.tdir, "--root", self.root],
                           env=env, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_the_env_var_names_the_real_target_dir(self):
        real = os.path.join(self.tmp, "fleet", "target")
        os.environ["CARGO_TARGET_DIR"] = real
        self.addCleanup(os.environ.pop, "CARGO_TARGET_DIR", None)
        self.build_tdir(real)
        self.assertEqual(seed.resolve_tdir("rust-modules/target", "CARGO_TARGET_DIR", self.root), real)
        self.assertEqual(seed.resolve_tdir("/abs/other", None, None), "/abs/other")
        os.environ["CARGO_TARGET_DIR"] = "rel/t"
        self.assertEqual(seed.resolve_tdir("x", "CARGO_TARGET_DIR", "/base"), "/base/rel/t")


@unittest.skipUnless(sys.platform == "darwin", "clonefile(2) is macOS-only")
class RealClone(unittest.TestCase):
    def test_clonefile_clones_a_tree_and_keeps_mtimes(self):
        base = tempfile.mkdtemp(prefix="plx-seed-clone.")
        self.addCleanup(shutil.rmtree, base, True)
        src = os.path.join(base, "src")
        touch(os.path.join(src, "a", "b.rlib"), "payload", mtime=1_234_567)
        dst = os.path.join(base, "dst")
        try:
            seed.clone_tree(src, dst)
        except OSError as e:
            self.skipTest(f"this volume cannot clone: {e}")
        with open(os.path.join(dst, "a", "b.rlib")) as fh:
            self.assertEqual(fh.read(), "payload")
        self.assertEqual(os.stat(os.path.join(dst, "a", "b.rlib")).st_mtime, 1_234_567)
        with self.assertRaises(OSError):
            seed.clone_tree(src, dst)  # an existing destination is refused, never merged


if __name__ == "__main__":
    unittest.main()
