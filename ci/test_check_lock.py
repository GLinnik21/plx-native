#!/usr/bin/env python3
"""Exercise `tools/check-lock.py`'s serialization, not `make check` itself: two workers
racing for one lock file, a killed holder, --timeout, and the PLX_CHECK_LOCK=off escape
hatch. No cargo, no network, under 5 s."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "tools/check-lock.py"


def run(lock_path, cmd, extra_args=(), env=None):
    # The wrapper treats PLX_CHECK_LOCK=off as "do not lock", and the documented way to bypass
    # `make check`'s own lock is to run `PLX_CHECK_LOCK=off make check`, which hands that variable to
    # this file too. A child that inherits it never locks, so three of the five tests failed under the
    # bypass. The children's environment is therefore built here, not inherited: the variable is
    # dropped, and only the test that wants it (`env=`) sets it back.
    full_env = os.environ.copy()
    full_env.pop("PLX_CHECK_LOCK", None)
    if env:
        full_env.update(env)
    return subprocess.Popen(
        [sys.executable, str(SCRIPT), "--lock", str(lock_path), *extra_args, "--", *cmd],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=full_env,
    )


class CheckLockTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="check-lock-tests-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.lock_path = self.root / "check.lock"

    def wait(self, proc, timeout=10):
        try:
            out, err = proc.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            proc.kill()
            out, err = proc.communicate()
            self.fail("process did not exit in time; stderr:\n" + err)
        return proc.returncode, out, err

    def test_two_concurrent_invocations_serialize(self):
        marker = self.root / "marker"
        # The first holder sleeps briefly while touching a marker file; the second
        # invocation's command only runs (and can only see the marker) once the first
        # has released the lock and exited.
        first = run(self.lock_path, ["python3", "-c",
                                      "import time,sys; time.sleep(1.5); "
                                      "open(sys.argv[1],'w').write('done')",
                                      str(marker)])
        time.sleep(0.3)  # let `first` win the race for the lock
        second = run(self.lock_path, ["python3", "-c",
                                       "import sys; sys.exit(0 if open(sys.argv[1]).read()"
                                       "=='done' else 1)",
                                       str(marker)])
        rc1, _out1, err1 = self.wait(first)
        rc2, _out2, err2 = self.wait(second)
        self.assertEqual(rc1, 0, err1)
        self.assertEqual(rc2, 0, "second invocation ran before the first released the lock: "
                          + err2)

    def test_exit_status_propagates(self):
        proc = run(self.lock_path, ["python3", "-c", "import sys; sys.exit(7)"])
        rc, _out, err = self.wait(proc)
        self.assertEqual(rc, 7, err)

    def test_holder_killed_unblocks_waiter(self):
        holder = run(self.lock_path, ["python3", "-c", "import time; time.sleep(60)"])
        # Wait until the holder has actually acquired (its own stderr says so) before
        # killing it, so this proves "killed holder releases" rather than "no holder yet".
        deadline = time.monotonic() + 5
        acquired = False
        while time.monotonic() < deadline:
            line = holder.stderr.readline()
            if "acquired" in line:
                acquired = True
                break
        self.assertTrue(acquired, "holder never reported acquiring the lock")
        holder.kill()
        holder.wait(timeout=5)
        holder.stdout.close()
        holder.stderr.close()

        waiter = run(self.lock_path, ["python3", "-c", "import sys; sys.exit(0)"])
        start = time.monotonic()
        rc, _out, err = self.wait(waiter, timeout=10)
        elapsed = time.monotonic() - start
        self.assertEqual(rc, 0, err)
        self.assertLess(elapsed, 8, "waiter took too long to acquire after SIGKILL: " + err)

    def test_timeout_exits_75_and_names_the_holder(self):
        holder = run(self.lock_path, ["python3", "-c", "import time; time.sleep(10)"])
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            line = holder.stderr.readline()
            if "acquired" in line:
                break
        try:
            waiter = run(self.lock_path, ["python3", "-c", "import sys; sys.exit(0)"],
                         extra_args=["--timeout", "1"])
            rc, _out, err = self.wait(waiter, timeout=10)
            self.assertEqual(rc, 75, err)
            self.assertIn("timed out", err)
            self.assertIn(str(holder.pid), err)
        finally:
            holder.kill()
            holder.wait(timeout=5)
            holder.stdout.close()
            holder.stderr.close()

    def test_plx_check_lock_off_bypasses_locking(self):
        # Two invocations with PLX_CHECK_LOCK=off must not serialize on the SAME lock
        # path even though one is still "holding" it — the escape hatch really disables
        # locking rather than merely widening the poll interval.
        holder = run(self.lock_path, ["python3", "-c", "import time; time.sleep(3)"],
                     env={"PLX_CHECK_LOCK": "off"})
        time.sleep(0.3)
        waiter = run(self.lock_path, ["python3", "-c", "import sys; sys.exit(0)"],
                     env={"PLX_CHECK_LOCK": "off"})
        start = time.monotonic()
        rc, _out, err = self.wait(waiter, timeout=5)
        elapsed = time.monotonic() - start
        self.assertEqual(rc, 0, err)
        self.assertLess(elapsed, 2, "waiter blocked even though PLX_CHECK_LOCK=off: " + err)
        holder.wait(timeout=5)
        holder.stdout.close()
        holder.stderr.close()


if __name__ == "__main__":
    unittest.main()
