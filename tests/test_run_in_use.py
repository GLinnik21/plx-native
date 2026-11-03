#!/usr/bin/env python3
"""`tests/run.py` must not drive a television the lock's pre-flight called IN USE.

`tools/tv-lock.sh acquire` exits 0 whatever the pre-flight reads and prints one `IN USE:` line at
column 0 when somebody looks to be watching. `with` already refuses to start its command on that
line; `acquire_tv_lock` used to run `acquire` with its stdout straight to the terminal and carry on
whenever the exit status was 0, so the whole suite ran on a set in use. Host tests, no TV: the
lock script is a fake in a temp dir that answers `acquire`, `status` and `release` and records the
words it was called with. What is pinned:

* an `IN USE:` line stops the run (SystemExit naming `PLX_TV_IN_USE_OK=1`), the lease just taken is
  released, and the operator still saw every line acquire printed;
* `PLX_TV_IN_USE_OK=1` (the owner said go) proceeds, keeps the lease and releases nothing;
* an IDLE or UNKNOWN verdict, and an `IN USE:` that is not at column 0, proceed;
* a lease this lane already holds is inherited without calling `acquire` at all, but `status` (which
  prints the pre-flight verdict too) is read again: an `IN USE:` line in it stops the run the same
  way, naming `PLX_TV_IN_USE_OK=1`, and releases nothing (the lane took that lease for its own
  reasons, so `_LOCK_TAKEN` stays False and the lease stays held); the override, or an IDLE or
  UNKNOWN verdict, proceeds.
"""
import contextlib
import io
import os
import sys
import tempfile
import unittest
from unittest import mock

TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, TESTS_DIR)
import run  # noqa: E402

FAKE_LOCK = """#!/bin/sh
echo "$1" >> "$FAKE_DIR/calls"
case "$1" in
  acquire) cat "$FAKE_DIR/acquire.out"; exit 0 ;;
  status) cat "$FAKE_DIR/status.out" ;;
esac
exit 0
"""


class InUsePreflight(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = tmp.name
        script = os.path.join(self.dir, "tv-lock.sh")
        with open(script, "w", encoding="utf-8") as f:
            f.write(FAKE_LOCK)
        os.chmod(script, 0o755)
        env = mock.patch.dict(os.environ, {"FAKE_DIR": self.dir})
        env.start()
        self.addCleanup(env.stop)
        os.environ.pop("PLX_TV_IN_USE_OK", None)
        for patch in (mock.patch.object(run, "TVLOCK", script),
                      mock.patch.object(run, "_LOCK_TAKEN", False)):
            patch.start()
            self.addCleanup(patch.stop)
        self.say("status", "no lease held\n")

    def say(self, word, text):
        with open(os.path.join(self.dir, word + ".out"), "w", encoding="utf-8") as f:
            f.write(text)

    def calls(self):
        try:
            with open(os.path.join(self.dir, "calls"), encoding="utf-8") as f:
                return f.read().split()
        except FileNotFoundError:
            return []

    def acquire(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            run.acquire_tv_lock("tv.example", "unit test")
        return out.getvalue()

    def test_in_use_stops_the_run_and_gives_the_lease_back(self):
        line = "IN USE: com.example.stream: stop and tell the owner"
        self.say("acquire", "lease taken\n%s\nHEADS-UP: another line\n" % line)
        out = io.StringIO()
        with contextlib.redirect_stdout(out), self.assertRaises(SystemExit) as stop:
            run.acquire_tv_lock("tv.example", "unit test")
        self.assertIn("lease taken", out.getvalue())
        self.assertIn(line, out.getvalue())
        self.assertIn("HEADS-UP: another line", out.getvalue())
        self.assertIn("PLX_TV_IN_USE_OK=1", str(stop.exception))
        self.assertIn("owner", str(stop.exception))
        self.assertEqual(self.calls(), ["status", "acquire", "release"])
        self.assertFalse(run._LOCK_TAKEN)

    def test_the_override_runs_and_keeps_the_lease(self):
        self.say("acquire", "IN USE: com.example.stream: stop and tell the owner\n")
        with mock.patch.dict(os.environ, {"PLX_TV_IN_USE_OK": "1"}):
            out = self.acquire()
        self.assertIn("IN USE:", out)
        self.assertEqual(self.calls(), ["status", "acquire"])
        self.assertTrue(run._LOCK_TAKEN)

    def test_idle_unknown_and_a_quoted_line_proceed(self):
        for text in ("IDLE: nothing in front\n", "UNKNOWN: panel unreadable\n",
                     "note: the old IN USE: line was cleared\n  IN USE: indented is not a verdict\n"):
            with self.subTest(text=text):
                if self.calls():
                    os.remove(os.path.join(self.dir, "calls"))
                self.say("acquire", text)
                self.assertIn(text.splitlines()[0], self.acquire())
                self.assertEqual(self.calls(), ["status", "acquire"])
                self.assertTrue(run._LOCK_TAKEN)
                run._LOCK_TAKEN = False

    def test_a_lease_this_lane_holds_is_inherited_when_the_set_is_not_in_use(self):
        self.say("acquire", "IN USE: com.example.stream: stop and tell the owner\n")
        for text in ("lease: HELD BY THIS LANE\n",
                     "lease: HELD BY THIS LANE\n  \u00b7 IDLE (LG Home launcher)\n",
                     "lease: HELD BY THIS LANE\n  ! UNKNOWN (the pre-flight read timed out after 10s)\n",
                     "lease: HELD BY THIS LANE\n  IN USE: indented is not a verdict\n"):
            with self.subTest(text=text):
                if self.calls():
                    os.remove(os.path.join(self.dir, "calls"))
                self.say("status", text)
                self.assertIn("inheriting", self.acquire())
                self.assertEqual(self.calls(), ["status"])
                self.assertFalse(run._LOCK_TAKEN)

    def test_an_inherited_lease_on_an_in_use_set_stops_the_run_and_keeps_the_lease(self):
        line = "IN USE: com.example.stream: stop and tell the owner"
        self.say("status", "== TV lock: the TV\nlease: HELD BY THIS LANE\n%s\n" % line)
        self.say("acquire", "lease taken\n")
        out = io.StringIO()
        with contextlib.redirect_stdout(out), self.assertRaises(SystemExit) as stop:
            run.acquire_tv_lock("tv.example", "unit test")
        self.assertIn(line, out.getvalue())
        self.assertNotIn("inheriting", out.getvalue())
        self.assertIn("PLX_TV_IN_USE_OK=1", str(stop.exception))
        self.assertIn("owner", str(stop.exception))
        self.assertIn("still held", str(stop.exception))
        self.assertEqual(self.calls(), ["status"])      # nothing taken, so nothing released
        self.assertFalse(run._LOCK_TAKEN)

    def test_the_override_inherits_an_in_use_lease_and_releases_nothing(self):
        self.say("status", "lease: HELD BY THIS LANE\nIN USE: com.example.stream: stop and tell the owner\n")
        with mock.patch.dict(os.environ, {"PLX_TV_IN_USE_OK": "1"}):
            out = self.acquire()
        self.assertIn("inheriting", out)
        self.assertEqual(self.calls(), ["status"])
        self.assertFalse(run._LOCK_TAKEN)


if __name__ == "__main__":
    unittest.main()
