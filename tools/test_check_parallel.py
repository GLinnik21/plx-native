#!/usr/bin/env python3
"""Self-test for tools/check-parallel.py: ordering, isolation, failure and the job cap.

No cargo, no network; every branch is a one-line `python3 -c` script, and the whole file runs in a
few seconds."""
import contextlib
import importlib.util
import io
import os
import re
import shlex
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

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


def step(script):
    """One manifest line that runs `script` under this interpreter."""
    return shlex.join([sys.executable, "-c", script])


def run_steps(lines, *args, env=None, timeout=60):
    """Run a manifest made of `lines` through `--steps`; returns (CompletedProcess, seconds)."""
    with tempfile.TemporaryDirectory() as tmp:
        manifest = os.path.join(tmp, "steps.txt")
        with open(manifest, "w") as handle:
            handle.write("\n".join(lines) + "\n")
        started = time.monotonic()
        r = subprocess.run([sys.executable, TOOL, *args, "--steps", manifest], capture_output=True, text=True,
                           timeout=timeout, env=dict(os.environ, **(env or {})))
        return r, time.monotonic() - started


class StepMode(unittest.TestCase):
    """`--steps FILE`: many short independent commands, each printed whole when it ends, none skipped
    when one fails, and a summary that names every failure."""

    def test_comments_and_blank_lines_are_not_steps(self):
        r, _ = run_steps(["# a comment", "", step("print('only-one')"), "   # indented comment"])
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(r.stdout.count("==== step:"), 1, r.stdout)
        self.assertIn("1 steps — all ok", r.stdout)

    def test_each_step_is_printed_whole_when_it_ends_not_in_manifest_order(self):
        slow = step("import time; time.sleep(0.8); print('slow-1'); print('slow-2')")
        fast = step("print('fast-1'); print('fast-2')")
        r, took = run_steps([slow, fast], "--jobs", "2")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertLess(r.stdout.index("fast-1"), r.stdout.index("slow-1"), "the fast step waited for the slow one")
        self.assertEqual(r.stdout.count("slow-1\nslow-2\n"), 1, r.stdout)
        self.assertEqual(r.stdout.count("fast-1\nfast-2\n"), 1, r.stdout)
        self.assertLess(took, 5)

    def test_a_banner_carries_the_name_the_status_and_both_times(self):
        burn = step("import time; t = time.process_time(); exec('while time.process_time() - t < 0.3: pass'); print('burned')")
        r, _ = run_steps([burn])
        self.assertRegex(r.stdout, r"==== step: .*burned.* — ok \(\d+\.\ds, cpu \d+\.\ds\) ====")
        cpu = float(re.search(r"cpu (\d+\.\d)s\)", r.stdout).group(1))
        self.assertGreaterEqual(cpu, 0.2, "CPU time is the step's, measured from its own process tree")

    def test_steps_overlap_up_to_jobs_and_no_further(self):
        sleeper = lambda tag: step(f"import time; time.sleep(0.8); print('{tag}')")
        _, together = run_steps([sleeper("a"), sleeper("b")], "--jobs", "2")
        _, in_turn = run_steps([sleeper("a"), sleeper("b")], "--jobs", "1")
        self.assertLess(together, 1.5, "two 0.8 s steps took as long as running them in turn")
        self.assertGreater(in_turn, 1.5)

    def test_a_failure_does_not_stop_the_other_steps_and_is_named_at_the_end(self):
        bad = step("import sys; print('the gate said no'); sys.exit(3)")
        later = step("import time; time.sleep(0.6); print('ran-after-the-failure')")
        also = step("import sys; print('second red'); sys.exit(5)")
        r, _ = run_steps([bad, later, also], "--jobs", "2")
        self.assertEqual(r.returncode, 3, "the first failing step in manifest order sets the exit code\n" + r.stdout)
        self.assertIn("ran-after-the-failure", r.stdout, "a failing step must not skip the rest")
        self.assertIn("the gate said no", r.stdout)
        self.assertIn("second red", r.stdout)
        self.assertIn("2 of 3 steps FAILED", r.stdout)
        tail = r.stdout[r.stdout.index("2 of 3 steps FAILED"):]
        self.assertRegex(tail, r"FAILED \(exit 3\): .*the gate said no")
        self.assertRegex(tail, r"FAILED \(exit 5\): .*second red")
        self.assertNotIn("ran-after-the-failure", tail, "the summary names failures only")

    def test_a_step_killed_by_a_signal_fails_the_run_with_a_shell_style_code(self):
        r, _ = run_steps([step("import os, signal; os.kill(os.getpid(), signal.SIGKILL)")])
        self.assertEqual(r.returncode, 137, r.stdout + r.stderr)
        self.assertIn("FAILED", r.stdout)

    def test_a_step_that_cannot_start_fails_the_run(self):
        r, _ = run_steps(["/nonexistent/definitely-not-a-program --flag"])
        self.assertEqual(r.returncode, 127, r.stdout + r.stderr)
        self.assertIn("cannot start", r.stdout)

    def test_stderr_is_part_of_a_steps_output(self):
        r, _ = run_steps([step("import sys; print('on-stderr', file=sys.stderr)")])
        self.assertIn("on-stderr", r.stdout)

    def test_a_step_listed_twice_is_refused(self):
        line = step("print('x')")
        r, _ = run_steps([line, line])
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("twice", r.stderr)

    def test_an_empty_manifest_is_refused(self):
        r, _ = run_steps(["# nothing"])
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("lists no steps", r.stderr)

    def test_steps_and_branches_cannot_be_mixed(self):
        with tempfile.NamedTemporaryFile("w", suffix=".txt") as manifest:
            manifest.write(step("pass") + "\n")
            manifest.flush()
            r = subprocess.run([sys.executable, TOOL, "--steps", manifest.name, branch("a", "pass")],
                               capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("different modes", r.stderr)

    def test_the_job_count_is_reported_and_the_env_override_wins(self):
        r, _ = run_steps([step("pass")], env={"PLX_CHECK_STEP_JOBS": "3"})
        self.assertIn("3 at once", r.stdout, r.stdout)
        r, _ = run_steps([step("pass")], "--jobs", "1", env={"PLX_CHECK_STEP_JOBS": "3"})
        self.assertIn("1 at once", r.stdout, "an explicit --jobs outranks the environment")

    def test_auto_jobs_follows_the_cores_between_two_and_the_cap(self):
        spec = importlib.util.spec_from_file_location("check_parallel", TOOL)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertEqual(module.auto_jobs(cpus=1), 2)
        self.assertEqual(module.auto_jobs(cpus=2), 2)
        self.assertEqual(module.auto_jobs(cpus=3), 3)
        self.assertEqual(module.auto_jobs(cpus=64), module.STEP_JOBS_CAP)
        self.assertEqual(module.auto_jobs(cpus=64, override="1"), 1)
        self.assertEqual(module.auto_jobs(cpus=1, override="9"), 9)
        with self.assertRaises(SystemExit):
            module.auto_jobs(cpus=4, override="many")

    def test_an_interrupted_run_stops_every_step_and_shows_the_partial_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            pidfile = os.path.join(tmp, "pids")
            script = os.path.join(tmp, "long.py")
            with open(script, "w") as handle:
                handle.write(LONG_BRANCH)
            long_step = shlex.join([sys.executable, script, pidfile])
            manifest = os.path.join(tmp, "steps.txt")
            with open(manifest, "w") as handle:
                handle.write(long_step + "\n" + step("print('quick-done')") + "\n")
            runner = subprocess.Popen([sys.executable, TOOL, "--jobs", "2", "--steps", manifest],
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                      start_new_session=True)
            pids = []
            try:
                self.assertTrue(wait_for(lambda: os.path.exists(pidfile), 20), "the step never started")
                with open(pidfile) as f:
                    pids = [int(p) for p in f.read().split()]
                os.killpg(runner.pid, signal.SIGTERM)
                out, _ = runner.communicate(timeout=30)
                self.assertTrue(wait_for(lambda: not any(alive(p) for p in pids), 5), f"left running: {pids}")
            finally:
                runner.kill()
                runner.wait()
                for pid in pids:
                    if alive(pid):
                        os.kill(pid, signal.SIGKILL)
        self.assertEqual(runner.returncode, 128 + signal.SIGTERM)
        self.assertIn("quick-done", out)
        self.assertIn("branch-output-so-far", out)
        self.assertIn("interrupted", out)


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def wait_for(predicate, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.05)
    return predicate()


# A branch that leaves a grandchild behind it (as cargo leaves rustc and the test binaries), says
# so on stdout, and reports both pids through a file.
LONG_BRANCH = (
    "import os, subprocess, sys, time\n"
    "child = subprocess.Popen(['sleep', '60'])\n"
    "print('branch-output-so-far', flush=True)\n"
    "open(sys.argv[1] + '.tmp', 'w').write(f'{os.getpid()} {child.pid}')\n"
    "os.rename(sys.argv[1] + '.tmp', sys.argv[1])\n"
    "time.sleep(60)\n"
)


class Interrupted(unittest.TestCase):
    """The runner puts each branch in its own session, so a signal to the runner does not reach the
    compiler by itself; every way the runner can be told to stop must stop the branches too."""

    def interrupt(self, signum):
        with tempfile.TemporaryDirectory() as tmp:
            pidfile = os.path.join(tmp, "pids")
            script = LONG_BRANCH.replace("sys.argv[1]", repr(pidfile))
            runner = subprocess.Popen(
                [sys.executable, TOOL, branch("long", script), branch("quick", "print('quick-done')")],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
            pids = []
            try:
                self.assertTrue(wait_for(lambda: os.path.exists(pidfile), 20), "the branch never started")
                with open(pidfile) as f:
                    pids = [int(p) for p in f.read().split()]
                os.killpg(runner.pid, signum)
                out, err = runner.communicate(timeout=30)
                self.assertTrue(wait_for(lambda: not any(alive(p) for p in pids), 5),
                                f"signal {signum} left a branch process running: {pids}")
            finally:
                runner.kill()
                runner.wait()
                for pid in pids:
                    if alive(pid):
                        os.kill(pid, signal.SIGKILL)
            return runner.returncode, out, err

    def test_every_termination_signal_stops_the_branches(self):
        for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGQUIT, signal.SIGINT):
            with self.subTest(signal=signal.Signals(signum).name):
                code, _, _ = self.interrupt(signum)
                self.assertGreater(code, 0, "an interrupted run must not look like a pass")

    def test_an_interrupted_run_still_shows_what_the_branches_printed(self):
        for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
            with self.subTest(signal=signal.Signals(signum).name):
                _, out, _ = self.interrupt(signum)
                self.assertIn("branch-output-so-far", out)
                self.assertIn("interrupted", out, "partial output must be marked as partial")
                # Fixed (command-line) order, and a branch that never produced anything is not
                # mislabelled as a pass.
                self.assertLess(out.index("check: long"), out.index("check: quick"), out)

    def test_a_sibling_that_exited_before_the_stop_is_labelled_by_what_it_did(self):
        spec = importlib.util.spec_from_file_location("check_parallel", TOOL)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        bad = module.Branch("bad", shlex.join([sys.executable, "-c", "import sys; sys.exit(1)"]))
        late = module.Branch("late", shlex.join([sys.executable, "-c", "print('late-output')"]))
        seen = set()
        real_poll = module.Branch.poll

        def poll(self):
            # Both have exited by the time they are polled, but `late` is reported as still running
            # once, which is what a sibling finishing between the poll loop and stop() looks like.
            self.proc.wait()
            if self is late and late.name not in seen:
                seen.add(late.name)
                return None
            return real_poll(self)

        with mock.patch.object(module.Branch, "poll", poll), contextlib.redirect_stdout(io.StringIO()) as out:
            code = module.run([bad, late], 2)
        bad.log.close()
        late.log.close()
        self.assertEqual(code, 1)
        self.assertNotIn("not started", out.getvalue())
        self.assertIn("late — ok", out.getvalue())


if __name__ == "__main__":
    unittest.main()
