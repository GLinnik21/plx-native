#!/usr/bin/env python3
"""Host regressions for the empty per-install state directory in an .ipk."""

import io
import os
from pathlib import Path
import shutil
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import flavor  # noqa: E402
import mkipk  # noqa: E402
from mkipk import state_archive_errors  # noqa: E402


class StatePackageTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="plx-state-package-"))
        self.data = self.root / "data"
        self.repo = self.root / "repo"
        (self.repo / "pkg").mkdir(parents=True)
        (self.data / "usr/palm/applications").mkdir(parents=True)

    def tearDown(self):
        shutil.rmtree(self.root)

    def _stage(self, flav="stable"):
        app = flavor.appinfo_for(flav)
        appdir = self.data / "usr/palm/applications" / app["id"]
        appdir.mkdir()
        return app, appdir

    def test_every_flavour_gets_empty_state_directory_with_runtime_metadata(self):
        for flav in flavor.FLAVORS:
            with self.subTest(flavour=flav):
                app, appdir = self._stage(flav)
                mkipk.stage_state(self.repo, self.data, app)
                state = appdir / "state"
                self.assertTrue(state.is_dir())
                self.assertEqual(list(state.iterdir()), [])
                self.assertEqual(state.stat().st_mode & 0o777, 0o775)
                archive = self.root / f"{flav}.tar.gz"
                mkipk.write_targz(archive, self.data, "")
                with tarfile.open(archive, "r:gz") as tf:
                    member = tf.getmember(f"usr/palm/applications/{app['id']}/state")
                    self.assertEqual((member.uid, member.gid, member.mode), (0, 5000, 0o775))
                    self.assertEqual(member.gname, "")

    def test_nonempty_source_state_is_rejected(self):
        app, _ = self._stage()
        source = self.repo / "pkg/state"
        source.mkdir()
        (source / "stale.json").write_text("{}")
        with self.assertRaises(SystemExit):
            mkipk.stage_state(self.repo, self.data, app)

    def test_source_state_symlink_is_rejected_without_touching_target(self):
        app, _ = self._stage()
        sentinel = self.root / "source-sentinel"
        sentinel.mkdir()
        marker = sentinel / "must-stay"
        marker.write_text("untouched")
        os.symlink(sentinel, self.repo / "pkg/state")
        with self.assertRaises(SystemExit):
            mkipk.stage_state(self.repo, self.data, app)
        self.assertEqual(marker.read_text(), "untouched")

    def test_staged_state_symlink_is_rejected_without_touching_target(self):
        app, appdir = self._stage()
        sentinel = self.root / "staged-sentinel"
        sentinel.mkdir()
        marker = sentinel / "must-stay"
        marker.write_text("untouched")
        os.symlink(sentinel, appdir / "state")
        with self.assertRaises(SystemExit):
            mkipk.stage_state(self.repo, self.data, app)
        self.assertEqual(marker.read_text(), "untouched")

    def test_archive_bytes_and_state_metadata_are_deterministic(self):
        app, appdir = self._stage("stable")
        (appdir / "plxnative").write_bytes(b"binary")
        (appdir / "readme.txt").write_bytes(b"text")
        (appdir / "nested").mkdir()
        mkipk.stage_state(self.repo, self.data, app)
        first = self.root / "one.tar.gz"
        second = self.root / "two.tar.gz"
        mkipk.write_targz(first, self.data, "")
        mkipk.write_targz(second, self.data, "")
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with tarfile.open(first, "r:gz") as tf:
            member = tf.getmember(f"usr/palm/applications/{app['id']}/state")
            self.assertTrue(member.isdir())
            self.assertEqual(member.uid, 0)
            self.assertEqual(member.gid, 5000)
            self.assertEqual(member.mode, 0o775)
            self.assertEqual(tf.getmember(f"usr/palm/applications/{app['id']}/plxnative").mode, 0o755)
            self.assertEqual(tf.getmember(f"usr/palm/applications/{app['id']}/readme.txt").mode, 0o644)
            self.assertEqual(tf.getmember(f"usr/palm/applications/{app['id']}/nested").mode, 0o755)

    def test_archive_checker_rejects_wrong_state_mode_or_gid(self):
        app, _ = self._stage()
        for mode, gid in ((0o755, 5000), (0o4775, 5000), (0o2775, 5000), (0o775, 0)):
            with self.subTest(mode=oct(mode), gid=gid):
                raw = io.BytesIO()
                with tarfile.open(fileobj=raw, mode="w:gz") as tf:
                    ti = tarfile.TarInfo(f"usr/palm/applications/{app['id']}/state")
                    ti.type = tarfile.DIRTYPE
                    ti.mode = mode
                    ti.uid = 0
                    ti.gid = gid
                    tf.addfile(ti)
                errors = state_archive_errors(raw.getvalue(), app["id"])
                self.assertTrue(errors)

    def test_archive_checker_rejects_nonempty_state(self):
        app, _ = self._stage()
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode="w:gz") as tf:
            state = tarfile.TarInfo(f"usr/palm/applications/{app['id']}/state")
            state.type = tarfile.DIRTYPE
            state.mode, state.uid, state.gid = 0o775, 0, 5000
            tf.addfile(state)
            payload = b"runtime data"
            child = tarfile.TarInfo(f"usr/palm/applications/{app['id']}/state/token")
            child.size = len(payload)
            child.mode, child.uid, child.gid = 0o644, 0, 0
            tf.addfile(child, io.BytesIO(payload))
        errors = state_archive_errors(raw.getvalue(), app["id"])
        self.assertTrue(errors)


if __name__ == "__main__":
    unittest.main()
