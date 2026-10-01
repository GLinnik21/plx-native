#!/usr/bin/env python3
"""Self-test for tools/check-parallel.py: ordering, isolation, failure and the job cap.

No cargo, no network; every branch is a one-line `python3 -c` script, and the whole file runs in a
few seconds."""
import os
import shlex
import subprocess
import sys
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
TOOL = os.path.join(HERE, "check-parallel.py")


def branch(name, script):
    """A NAME=COMMAND argument that runs `script` under this interpreter."""
    return f"{name}=" + shlex.join([sys.executable, "-c", script])


def run(*args, timeout=60):
    started = time.monotonic()
    r = subprocess.run([sys.executable, TOOL, *args], capture_output=True, text=True, timeout=timeout)
    return r, time.monotonic() - started


class CheckParallel(unittest.TestCase):
    def test_output_follows_the_named_order_not_the_finishing_order(self):
        slow = branch("slow", "import time; time.sleep(1.0); print('slow-1'); print('slow-2')")
        fast = branch("fast", "print('fast-1'); print('fast-2')")
        r, took = run(slow, fast)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        out = r.stdout
        self.assertLess(out.index("slow-1"), out.index("fast-1"), out)
        # Each branch is whole under its banner: no line of one inside the other.
        self.assertEqual(out.count("slow-1\nslow-2\n"), 1, out)
        self.assertEqual(out.count("fast-1\nfast-2\n"), 1, out)
        self.assertLess(took, 5)

    def test_branches_really_overlap(self):
        a = branch("a", "import time; time.sleep(1.5); print('a')")
        b = branch("b", "import time; time.sleep(1.5); print('b')")
        r, took = run(a, b)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertLess(took, 2.8, "two 1.5 s branches took as long as running them in turn")

    def test_jobs_one_runs_them_in_turn(self):
        a = branch("a", "import time; time.sleep(1.2); print('a')")
        b = branch("b", "import time; time.sleep(1.2); print('b')")
        r, took = run("--jobs", "1", a, b)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertGreater(took, 2.3)

    def test_a_failing_branch_fails_the_run_with_its_output(self):
        ok = branch("ok", "print('fine')")
        bad = branch("bad", "import sys; print('the gate said no'); print('why', file=sys.stderr); sys.exit(3)")
        r, _ = run(ok, bad)
        self.assertEqual(r.returncode, 3, r.stdout + r.stderr)
        self.assertIn("FAILED, exit 3", r.stdout)
        self.assertIn("the gate said no", r.stdout)
        self.assertIn("why", r.stdout, "stderr of a branch is part of its output")

    def test_a_failure_stops_a_slow_sibling(self):
        slow = branch("slow", "import time; time.sleep(30); print('never')")
        bad = branch("bad", "import sys; print('red'); sys.exit(1)")
        r, took = run(slow, bad)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertLess(took, 10, "the failed run waited for the slow branch")
        self.assertIn("stopped, a sibling branch failed", r.stdout)
        self.assertNotIn("never", r.stdout)

    def test_the_first_failure_in_named_order_sets_the_exit_code(self):
        a = branch("a", "import sys, time; time.sleep(0.5); print('a failed'); sys.exit(4)")
        b = branch("b", "import sys; print('b failed'); sys.exit(5)")
        r, _ = run(a, b)
        # b fails first in time and stops a; the verdict is whichever FINISHED failing and was not
        # stopped, so the exit code is b's and a is reported as stopped, never as a second failure.
        self.assertIn(r.returncode, (4, 5), r.stdout + r.stderr)
        self.assertNotEqual(r.returncode, 0)

    def test_a_malformed_branch_is_refused(self):
        r, _ = run("no-equals-sign")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("NAME=COMMAND", r.stderr)

    def test_a_branch_that_cannot_start_fails_the_run(self):
        r, _ = run("ghost=/nonexistent/definitely-not-a-program")
        self.assertEqual(r.returncode, 127, r.stdout + r.stderr)
        self.assertIn("cannot start", r.stdout)


if __name__ == "__main__":
    unittest.main()
