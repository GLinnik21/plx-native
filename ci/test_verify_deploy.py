#!/usr/bin/env python3
"""Tests for `ci/verify-deploy.py`, the host half of `make deploy`'s payload check.

The bug it exists for was NOT a wrong hash — it was a file the recipe never sent at all
(`pkg/splash.png`), which is why `test_missing_on_device` is the one case here that matters most:
a script that only ever compares hashes for names the device happens to mention would silently
pass over exactly that failure, the same way `deploy` itself did for weeks.
"""
from __future__ import annotations

import hashlib
import importlib.util
import pathlib
import tempfile
import unittest

_spec = importlib.util.spec_from_file_location(
    "verify_deploy", pathlib.Path(__file__).with_name("verify-deploy.py")
)
assert _spec and _spec.loader
vd = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(vd)


class ParseRemote(unittest.TestCase):
    def test_plain_busybox_line(self):
        text = "d41d8cd98f00b204e9800998ecf8427e  splash.png\n"
        self.assertEqual(vd.parse_remote(text), {"splash.png": "d41d8cd98f00b204e9800998ecf8427e"})

    def test_gnu_binary_marker_tolerated(self):
        text = "d41d8cd98f00b204e9800998ecf8427e *splash.png\n"
        self.assertEqual(vd.parse_remote(text), {"splash.png": "d41d8cd98f00b204e9800998ecf8427e"})

    def test_missing_file_line_is_dropped_not_matched(self):
        # busybox's own "No such file or directory" line must never be mistaken for a hash.
        text = "splash.png: No such file or directory\n"
        self.assertEqual(vd.parse_remote(text), {})

    def test_multiple_lines(self):
        text = (
            "d41d8cd98f00b204e9800998ecf8427e  a.png\n"
            "0cc175b9c0f1b6a831c399e269772661  b.ttf\n"
        )
        self.assertEqual(
            vd.parse_remote(text),
            {"a.png": "d41d8cd98f00b204e9800998ecf8427e", "b.ttf": "0cc175b9c0f1b6a831c399e269772661"},
        )


class Check(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = pathlib.Path(self.tmp.name) / "splash.png"
        self.path.write_bytes(b"some launch image bytes")
        self.local_hash = hashlib.md5(self.path.read_bytes()).hexdigest()

    def test_matching_hash_passes(self):
        remote = f"{self.local_hash}  splash.png\n"
        self.assertEqual(vd.check([str(self.path)], remote), [])

    def test_stale_device_copy_is_a_mismatch(self):
        # The exact shape of the reported bug: the device has SOME file under that name, an
        # OLDER one, so a name-only presence check would wrongly call this healthy.
        remote = "ffffffffffffffffffffffffffffffff  splash.png\n"
        failures = vd.check([str(self.path)], remote)
        self.assertEqual(len(failures), 1)
        self.assertIn("MISMATCH", failures[0])

    def test_missing_on_device(self):
        # Nothing in the remote listing names this file at all — the actual failure mode that
        # went unnoticed for weeks (deploy never scp'd it, so the device's `md5sum` on that
        # basename fails and its stderr line carries no hash).
        failures = vd.check([str(self.path)], "splash.png: No such file or directory\n")
        self.assertEqual(len(failures), 1)
        self.assertIn("MISSING", failures[0])

    def test_missing_locally_is_reported_not_crashed(self):
        failures = vd.check([str(pathlib.Path(self.tmp.name) / "nope.png")], "")
        self.assertEqual(len(failures), 1)
        self.assertIn("not found locally", failures[0])

    def test_several_files_report_every_failure(self):
        other = pathlib.Path(self.tmp.name) / "icon.png"
        other.write_bytes(b"icon bytes")
        other_hash = hashlib.md5(other.read_bytes()).hexdigest()
        remote = f"{self.local_hash}  splash.png\n{other_hash}  icon.png\n"
        self.assertEqual(vd.check([str(self.path), str(other)], remote), [])
        # Corrupt one of the two and the other must still be reported clean.
        bad_remote = f"ffffffffffffffffffffffffffffffff  splash.png\n{other_hash}  icon.png\n"
        failures = vd.check([str(self.path), str(other)], bad_remote)
        self.assertEqual(len(failures), 1)
        self.assertIn("splash.png", failures[0])


class Main(unittest.TestCase):
    def test_no_args_is_a_usage_error(self):
        self.assertEqual(vd.main(["verify-deploy.py"]), 2)


if __name__ == "__main__":
    unittest.main()
