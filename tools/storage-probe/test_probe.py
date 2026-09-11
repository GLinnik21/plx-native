#!/usr/bin/env python3
"""Host regressions for the storage probe's no-follow and trust checks."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SOURCE = Path(__file__).with_name("probe.c")


class StorageProbeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cc = shutil.which("cc")
        if cc is None:
            raise unittest.SkipTest("cc is not available")
        cls.build = Path(tempfile.mkdtemp(prefix="plx-storage-probe-build-"))
        cls.binary = cls.build / "probe"
        subprocess.run(
            [
                cc,
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-DPROBE_HEADLESS_ONLY",
                str(SOURCE),
                "-o",
                str(cls.binary),
            ],
            check=True,
        )

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.build, ignore_errors=True)

    @staticmethod
    def _root():
        root = Path(tempfile.mkdtemp(prefix="plx-storage-probe-root-"))
        os.chmod(root, 0o755)
        for name, mode in (("test0755", 0o755), ("test0775", 0o775), ("test0777", 0o777)):
            path = root / name
            path.mkdir()
            os.chmod(path, mode)
        return root

    def _run(self, root):
        return subprocess.run(
            [str(self.binary), "--headless", "--root", str(root)],
            check=True,
            capture_output=True,
            text=True,
            timeout=5,
        ).stdout

    def test_new_then_trusted_prior_is_reported_without_overwrite(self):
        root = self._root()
        try:
            first = self._run(root)
            for name in ("test0755", "test0775", "test0777"):
                section = first.split(f"[{name}]", 1)[1].split("[", 1)[0]
                self.assertIn("Prior: NEW (probe.dat absent)", section)
                self.assertIn("Create temp: OK", section)
                self.assertIn("Write marker: OK", section)
                self.assertIn("Rename: OK", section)
                self.assertIn("Fsync directory: OK", section)
                self.assertEqual((root / name / "probe.dat").stat().st_mode & 0o777, 0o600)

            second = self._run(root)
            for name in ("test0755", "test0775", "test0777"):
                section = second.split(f"[{name}]", 1)[1].split("[", 1)[0]
                self.assertIn("Prior: PRIOR FOUND (trusted marker)", section)
        finally:
            shutil.rmtree(root)

    def test_symlink_prior_is_refused_and_outside_sentinel_is_unchanged(self):
        root = self._root()
        try:
            sentinel = root / "outside"
            sentinel.write_bytes(b"keep me\n")
            os.symlink(sentinel, root / "test0755" / "probe.dat")
            output = self._run(root)
            section = output.split("[test0755]", 1)[1].split("[", 1)[0]
            self.assertIn("Prior: ERROR", section)
            self.assertIn("Create temp: SKIPPED (untrusted prior)", section)
            self.assertEqual(sentinel.read_bytes(), b"keep me\n")
        finally:
            shutil.rmtree(root)

    def test_symlink_candidate_directory_is_refused(self):
        root = self._root()
        try:
            outside = root / "outside-dir"
            outside.mkdir()
            (outside / "probe.dat").write_bytes(b"keep me\n")
            shutil.rmtree(root / "test0755")
            os.symlink(outside, root / "test0755")
            output = self._run(root)
            section = output.split("[test0755]", 1)[1].split("[", 1)[0]
            self.assertIn("type=other", section)
            self.assertIn("Prior: ERROR open-directory", section)
            self.assertEqual((outside / "probe.dat").read_bytes(), b"keep me\n")
        finally:
            shutil.rmtree(root)

    def test_untrusted_prior_forms_are_refused_without_replacement(self):
        cases = {
            "unknown": b"not the marker\n",
            "permissive": b"plxnative-storage-probe-v1\n",
        }
        for kind, contents in cases.items():
            with self.subTest(kind=kind):
                root = self._root()
                try:
                    prior = root / "test0755" / "probe.dat"
                    prior.write_bytes(contents)
                    os.chmod(prior, 0o600 if kind == "unknown" else 0o644)
                    before = prior.read_bytes()
                    output = self._run(root)
                    section = output.split("[test0755]", 1)[1].split("[", 1)[0]
                    self.assertIn("Prior: ERROR", section)
                    self.assertIn("Create temp: SKIPPED (untrusted prior)", section)
                    self.assertEqual(prior.read_bytes(), before)
                finally:
                    shutil.rmtree(root)

    def test_directory_and_fifo_priors_are_refused_without_blocking(self):
        for kind in ("directory", "fifo"):
            with self.subTest(kind=kind):
                root = self._root()
                try:
                    prior = root / "test0755" / "probe.dat"
                    if kind == "directory":
                        prior.mkdir()
                    else:
                        os.mkfifo(prior, 0o600)
                    output = self._run(root)
                    section = output.split("[test0755]", 1)[1].split("[", 1)[0]
                    self.assertIn("Prior: ERROR", section)
                    self.assertIn("Create temp: SKIPPED (untrusted prior)", section)
                finally:
                    shutil.rmtree(root)

    def test_missing_candidate_directory_is_reported(self):
        root = self._root()
        try:
            shutil.rmtree(root / "test0755")
            output = self._run(root)
            section = output.split("[test0755]", 1)[1].split("[", 1)[0]
            self.assertIn("Directory: ERROR lstat", section)
            self.assertIn("errno=2", section)
        finally:
            shutil.rmtree(root)


if __name__ == "__main__":
    unittest.main()
