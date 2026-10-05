#!/usr/bin/env python3
"""A timing run must not grade the fast-profile (`tvdev`) binary; a functional run may.

Host tests, no TV, no network: the television is a fake command runner. What is pinned:

* `tools/arm_profile_guard.py` reads the one-line `arm-profile` record `make deploy` writes next to
  the installed binary: `tvdev` is refused with a message naming `make ARM_PROFILE=release`,
  `release` is accepted, and a missing or garbled record is `unrecorded` and accepted (a binary
  deployed before the record existed or installed from a package is always a release build);
* `tests/run.py`: `do_build(timing=True)` puts `ARM_PROFILE=release` on BOTH make goals and
  `do_build()` for a functional run does not; `require_release_binary` exits on `tvdev` and returns
  the label otherwise; the FPS path builds with `timing=True`, checks the deployed binary and
  records the profile in its summary and in the graphics-profile bundle; the two functional paths
  still build with the fast default;
* `make deploy` writes the record; `tools/profile-graphics` and `tools/tv-sched-trace.sh` call the
  guard.
"""
import contextlib
import io
import os
import re
import sys
import types
import unittest
from unittest import mock

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(TESTS_DIR)
sys.path.insert(0, TESTS_DIR)
sys.path.append(os.path.join(REPO_ROOT, "tools"))
import arm_profile_guard as guard  # noqa: E402
import run  # noqa: E402


def read(*parts):
    with open(os.path.join(REPO_ROOT, *parts), encoding="utf-8") as f:
        return f.read()


def fake_tv(record):
    """A `run(command)` that answers the record read with `record` (None = no such file)."""
    seen = []

    def runner(command):
        seen.append(command)
        ok = record is not None and "arm-profile" in command
        return types.SimpleNamespace(stdout=record if ok else "", returncode=0 if ok else 1)
    runner.seen = seen
    return runner


class Guard(unittest.TestCase):
    def test_record_parsing(self):
        for text, want in (("tvdev\n", "tvdev"), ("release\n", "release"), (" TVDEV ", "tvdev"),
                           ("", None), ("fast\n", None), ("\n", None)):
            self.assertEqual(guard.parse_record(text), want, repr(text))

    def test_tvdev_is_refused_with_the_fix(self):
        profile, message = guard.check(fake_tv("tvdev\n"), "/apps/x", "com.example.debug")
        self.assertEqual(profile, "tvdev")
        self.assertIn("make ARM_PROFILE=release", message)
        self.assertIn("com.example.debug", message)

    def test_release_and_unrecorded_are_accepted(self):
        self.assertEqual(guard.check(fake_tv("release\n"), "/apps/x"), ("release", None))
        self.assertEqual(guard.check(fake_tv(None), "/apps/x"), ("unrecorded", None))
        self.assertEqual(guard.check(fake_tv("garbage\n"), "/apps/x"), ("unrecorded", None))

    def test_it_reads_the_app_directory_record(self):
        runner = fake_tv("release\n")
        guard.check(runner, "/media/apps/x")
        self.assertEqual(runner.seen, ["cat /media/apps/x/arm-profile 2>/dev/null"])

    def test_cli_exit_status(self):
        for record, status in (("tvdev\n", 1), ("release\n", 0), ("", 0)):
            with self.subTest(record=record):
                err = io.StringIO()
                with mock.patch.object(guard, "_make_query", return_value=("tv", "com.example", "/apps/x")), \
                        mock.patch.object(guard.subprocess, "run",
                                          return_value=types.SimpleNamespace(stdout=record)), \
                        contextlib.redirect_stderr(err):
                    self.assertEqual(guard.main([]), status)
                self.assertEqual("ARM_PROFILE=release" in err.getvalue(), status == 1)


class HarnessBuild(unittest.TestCase):
    def build_goals(self, **kw):
        calls = []

        def fake_make(target_args, timeout, capture=True):
            calls.append(list(target_args))
            return types.SimpleNamespace(returncode=0)
        with mock.patch.object(run, "make", fake_make), contextlib.redirect_stdout(io.StringIO()):
            run.do_build("tv.example", **kw)
        return calls

    def test_a_timing_build_asks_for_the_lto_profile_on_every_goal(self):
        calls = self.build_goals(timing=True)
        self.assertEqual([c[0] for c in calls], ["all", "deploy"])
        for call in calls:
            self.assertIn("ARM_PROFILE=release", call)

    def test_a_functional_build_keeps_the_fast_default(self):
        for call in self.build_goals():
            self.assertFalse([a for a in call if a.startswith("ARM_PROFILE")], call)

    def release_check(self, record):
        with mock.patch.object(run, "make_query", return_value="/apps/x"), \
                mock.patch.object(run, "FLAVOUR", "debug"), mock.patch.object(run, "APPID", "com.example.debug"), \
                contextlib.redirect_stdout(io.StringIO()):
            return run.require_release_binary("tv.example", runner=fake_tv(record))

    def test_a_deployed_tvdev_binary_stops_a_timing_run(self):
        with self.assertRaises(SystemExit) as cm:
            self.release_check("tvdev\n")
        self.assertIn("make ARM_PROFILE=release", str(cm.exception))

    def test_release_and_unrecorded_binaries_are_graded_and_labelled(self):
        self.assertEqual(self.release_check("release\n"), "release")
        self.assertEqual(self.release_check(None), "unrecorded")


class Wiring(unittest.TestCase):
    """Source-level pins for the call sites that a unit test cannot reach without a television."""

    @classmethod
    def setUpClass(cls):
        cls.run_py = read("tests", "run.py")

    def test_the_fps_path_builds_for_timing_checks_and_records(self):
        start = self.run_py.index("if args.fps or args.fps_player:")
        end = self.run_py.index("    if not cases:", start)
        branch = self.run_py[start:end]
        self.assertIn('do_build(cfg["tv"], timing=True)', branch)
        self.assertNotIn('do_build(cfg["tv"])', branch)
        self.assertRegex(branch, r'cfg\["arm_profile"\] = require_release_binary\(cfg\["tv"\]\)')
        # the check sits BEFORE the first scene or the graphics profile is launched
        self.assertLess(branch.index("require_release_binary"), branch.index("run_graphics_profile("))
        self.assertLess(branch.index("require_release_binary"), branch.index("run_fps_suite("))

    def test_the_functional_paths_still_build_with_the_default(self):
        self.assertEqual(self.run_py.count('do_build(cfg["tv"], timing=True)'), 1)
        self.assertEqual(len(re.findall(r'do_build\(cfg\["tv"\]\)', self.run_py)), 2)

    def test_the_profile_is_in_the_report(self):
        self.assertIn("binary profile: {cfg.get('arm_profile', 'unknown')}", self.run_py)
        self.assertIn('"arm_profile": cfg.get("arm_profile", "unknown")', self.run_py)

    def test_deploy_records_the_profile_next_to_the_binary(self):
        makefile = read("Makefile")
        recipe = makefile[makefile.index("\ndeploy: "):makefile.index("\nverify-deploy:")]
        self.assertRegex(recipe, r"printf .*\$\(ARM_PROFILE\).*> \$\(APPDIR\)/" + re.escape(guard.RECORD))

    def test_the_other_timing_tools_call_the_guard(self):
        self.assertIn("arm_profile_guard.check(", read("tools", "profile-graphics"))
        self.assertIn('"arm_profile": arm_profile', read("tools", "profile-graphics"))
        self.assertIn("arm_profile_guard.py", read("tools", "tv-sched-trace.sh"))


if __name__ == "__main__":
    unittest.main()
