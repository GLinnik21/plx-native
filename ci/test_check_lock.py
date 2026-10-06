#!/usr/bin/env python3
"""Exercise `tools/check-lock.py`, not `make check` itself: the slot lock (two shared runs at
once, a third waits and names both holders, a killed holder frees its slot, PLX_CHECK_SLOTS=1 is
the old one-at-a-time lock), exclusive mode for the benchmarks (blocks and is blocked by shared
runs, cannot be starved by a stream of shared requests, two exclusive requests do not deadlock),
the default slot count, --timeout, and the PLX_CHECK_LOCK=off escape hatch.

Hermetic: every test uses a temp lock directory, polls at 50 ms instead of 2 s, and runs no cargo
and no network. The children's environment is built here, never inherited (see `Run`)."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "tools/check-lock.py"
POLL = "0.05"

# One-line child programs.
SLEEP = "import time,sys; time.sleep(float(sys.argv[1]))"
# argv: logfile tag seconds lockbase -> appends "<tag> start|end <t>" around the sleep, and
# "<tag> admit <t>": the epoch at which its wrapper (the parent process) won the lock, read from the
# identity the wrapper wrote. `start` is later than `admit` by the interpreter's start-up time, which
# is long under load, so only `admit` can say whether a run got in before or after a request.
LOGGED = (
    "import glob,json,os,sys,time\n"
    "def log(w, t=None):\n"
    "    t = time.time() if t is None else t\n"
    "    open(sys.argv[1],'a').write('%s %s %.6f\\n' % (sys.argv[2], w, t))\n"
    "def admitted():\n"
    "    for p in glob.glob(sys.argv[4] + '*'):\n"
    "        try: d = json.load(open(p))\n"
    "        except ValueError: continue\n"
    "        if d.get('pid') == os.getppid() and 'epoch' in d: return d['epoch']\n"
    "log('admit', admitted()); log('start'); time.sleep(float(sys.argv[3])); log('end')\n"
)
# argv: mine theirs -> touches `mine`, then needs `theirs` to appear: only passes if the other
# run is alive at the same time.
BARRIER = (
    "import os,sys,time\n"
    "open(sys.argv[1],'w').close()\n"
    "end=time.time()+8\n"
    "while time.time()<end:\n"
    "    if os.path.exists(sys.argv[2]): sys.exit(0)\n"
    "    time.sleep(0.02)\n"
    "sys.exit(1)\n"
)


def child_env(extra):
    # The wrapper treats PLX_CHECK_LOCK=off as "do not lock", and the documented way to bypass
    # `make check`'s own lock is to run `PLX_CHECK_LOCK=off make check`, which hands that variable to
    # this file too: a child that inherits it never locks. PLX_CHECK_SLOTS and PLX_CHECK_POLL change
    # the semantics just as much. So all three are dropped and every test sets what it needs.
    env = os.environ.copy()
    for name in ("PLX_CHECK_LOCK", "PLX_CHECK_SLOTS", "PLX_CHECK_POLL"):
        env.pop(name, None)
    env["PLX_CHECK_POLL"] = POLL
    env.update(extra)
    return env


class Run:
    """One `check-lock.py` invocation. Its stderr goes to a file so a test can poll it without
    blocking on a pipe."""

    def __init__(self, root, lock_path, cmd, slots="2", exclusive=False, timeout=None, env=None):
        self.err_path = root / ("err-%d-%d" % (os.getpid(), id(self)))
        self.err_file = open(self.err_path, "w")
        args = [sys.executable, str(SCRIPT), "--lock", str(lock_path)]
        if timeout is not None:
            args += ["--timeout", str(timeout)]
        if exclusive:
            args.append("--exclusive")
        extra = {"PLX_CHECK_SLOTS": slots}
        extra.update(env or {})
        if extra.get("PLX_CHECK_SLOTS") is None:
            del extra["PLX_CHECK_SLOTS"]
        self.proc = subprocess.Popen(
            args + ["--", *cmd], stdout=subprocess.DEVNULL, stderr=self.err_file,
            text=True, env=child_env(extra))
        self.pid = self.proc.pid

    def err(self):
        return self.err_path.read_text()

    def wait_for(self, text, timeout=10):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if text in self.err():
                return time.time()
            if self.proc.poll() is not None and text not in self.err():
                break
            time.sleep(0.01)
        raise AssertionError("never saw %r in stderr:\n%s" % (text, self.err()))

    def finish(self, timeout=15):
        try:
            self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.kill()
            raise AssertionError("did not exit in time; stderr:\n" + self.err())
        self.err_file.close()
        return self.proc.returncode, self.err()

    def kill(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()
        self.err_file.close()


def read_log(path):
    """{tag: {"start": t, "end": t}} from a LOGGED log."""
    out = {}
    for line in Path(path).read_text().splitlines():
        tag, what, stamp = line.split()
        out.setdefault(tag, {})[what] = float(stamp)
    return out


class LockTestCase(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="check-lock-tests-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.lock_path = self.root / "check.lock"
        self.runs = []
        self.addCleanup(self.kill_all)

    def kill_all(self):
        for run in self.runs:
            run.kill()

    def start(self, cmd, **kwargs):
        run = Run(self.root, self.lock_path, cmd, **kwargs)
        self.runs.append(run)
        return run

    def hold(self, seconds=60, **kwargs):
        """A run that has acquired and is sitting in `sleep`."""
        run = self.start(["python3", "-c", SLEEP, str(seconds)], **kwargs)
        run.wait_for("acquired")
        return run

    def logged(self, log, tag, seconds, **kwargs):
        return self.start(["python3", "-c", LOGGED, str(log), tag, str(seconds), str(self.lock_path)], **kwargs)

    def wait_for_gate(self, run, timeout=10):
        gate = Path(str(self.lock_path) + ".gate")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                info = json.loads(gate.read_text())
                if info.get("pid") == run.pid:
                    return info["epoch"]  # when it won the gate
            except (OSError, ValueError):
                pass
            time.sleep(0.01)
        raise AssertionError("pid %d never took the gate" % run.pid)

    def assert_blocked(self, run_kwargs, *named):
        """A run with a 0.5 s timeout exits 75 and its message names every pid in `named`."""
        waiter = self.start(["python3", "-c", "import sys; sys.exit(0)"], timeout=0.5, **run_kwargs)
        rc, err = waiter.finish()
        self.assertEqual(rc, 75, err)
        self.assertIn("timed out", err)
        # The first poll already printed the holders; the timeout repeats them. Return the last line.
        last = err.strip().splitlines()[-1]
        for run in named:
            self.assertIn("pid %d " % run.pid, last)
        return last


class SharedSlotTests(LockTestCase):
    def test_two_shared_holders_run_concurrently(self):
        a, b = self.root / "a", self.root / "b"
        first = self.start(["python3", "-c", BARRIER, str(a), str(b)])
        second = self.start(["python3", "-c", BARRIER, str(b), str(a)])
        for run in (first, second):
            rc, err = run.finish()
            self.assertEqual(rc, 0, "the two runs were not alive at the same time:\n" + err)

    def test_third_waits_and_its_message_names_both_holders(self):
        a, b = self.hold(), self.hold()
        err = self.assert_blocked({}, a, b)
        self.assertEqual(err.count("pid "), 2, err)
        for needle in ("started", str(Path.cwd())):
            self.assertIn(needle, err)

    def test_waiter_prints_the_holders_immediately_not_only_at_timeout(self):
        a, b = self.hold(), self.hold()
        waiter = self.start(["python3", "-c", "import sys; sys.exit(0)"])
        waiter.wait_for("waiting on")
        text = waiter.err()
        self.assertIn("pid %d " % a.pid, text)
        self.assertIn("pid %d " % b.pid, text)
        # And the waiter goes in the moment a slot frees up.
        a.proc.kill()
        rc, err = waiter.finish()
        self.assertEqual(rc, 0, err)
        self.assertIn("acquired", err)

    def test_a_killed_holder_frees_its_slot_while_the_other_keeps_running(self):
        a, b = self.hold(), self.hold()
        waiter = self.start(["python3", "-c", "import sys; sys.exit(0)"])
        waiter.wait_for("waiting on")
        a.proc.kill()
        a.proc.wait()
        rc, err = waiter.finish()
        self.assertEqual(rc, 0, err)
        self.assertIsNone(b.proc.poll(), "the surviving holder must still be running")

    def test_slots_1_is_the_old_one_at_a_time_lock(self):
        marker = self.root / "marker"
        first = self.start(
            ["python3", "-c", "import time,sys; time.sleep(1.2); open(sys.argv[1],'w').write('done')",
             str(marker)], slots="1")
        first.wait_for("acquired")
        second = self.start(
            ["python3", "-c", "import sys; sys.exit(0 if open(sys.argv[1]).read()=='done' else 1)",
             str(marker)], slots="1")
        rc1, err1 = first.finish()
        rc2, err2 = second.finish()
        self.assertEqual(rc1, 0, err1)
        self.assertEqual(rc2, 0, "second run started before the first released the lock:\n" + err2)

    def test_slots_1_names_the_single_holder(self):
        holder = self.hold(slots="1")
        err = self.assert_blocked({"slots": "1"}, holder)
        self.assertEqual(err.count("pid "), 1, err)

    def test_the_slot_cap_follows_plx_check_slots(self):
        held = [self.hold(slots="3") for _ in range(3)]
        err = self.assert_blocked({"slots": "3"}, *held)
        self.assertEqual(err.count("pid "), 3, err)

    def test_a_bad_slot_count_is_refused(self):
        for bad in ("0", "-1", "two", "99"):
            run = self.start(["python3", "-c", "pass"], slots=bad)
            rc, err = run.finish()
            self.assertNotEqual(rc, 0, bad)
            self.assertIn("PLX_CHECK_SLOTS", err)

    def test_a_clean_exit_empties_the_identity_files(self):
        run = self.start(["python3", "-c", "pass"])
        rc, err = run.finish()
        self.assertEqual(rc, 0, err)
        for path in self.root.glob("check.lock*"):
            self.assertEqual(path.read_text(), "", str(path))


class SingleLockBehaviourTests(LockTestCase):
    """What the one-flock wrapper already guaranteed, unchanged."""

    def test_exit_status_propagates(self):
        rc, err = self.start(["python3", "-c", "import sys; sys.exit(7)"]).finish()
        self.assertEqual(rc, 7, err)

    def test_signal_death_is_128_plus_signal(self):
        run = self.start(["python3", "-c", "import os,signal; os.kill(os.getpid(), signal.SIGTERM)"])
        rc, err = run.finish()
        self.assertEqual(rc, 128 + signal.SIGTERM, err)

    def test_holder_killed_unblocks_waiter_with_one_slot(self):
        holder = self.hold(slots="1")
        waiter = self.start(["python3", "-c", "import sys; sys.exit(0)"], slots="1")
        waiter.wait_for("waiting on")
        holder.proc.kill()
        start = time.monotonic()
        rc, err = waiter.finish()
        self.assertEqual(rc, 0, err)
        self.assertLess(time.monotonic() - start, 5, err)

    def test_timeout_exits_75_and_names_the_holder(self):
        holder = self.hold(slots="1")
        err = self.assert_blocked({"slots": "1"}, holder)
        self.assertIn("timed out", err)

    def test_plx_check_lock_off_bypasses_locking(self):
        # Two invocations with PLX_CHECK_LOCK=off must not serialize on the SAME lock
        # path even though one is still "holding" it, and not even with one slot.
        holder = self.start(["python3", "-c", SLEEP, "1.5"], slots="1", env={"PLX_CHECK_LOCK": "off"})
        time.sleep(0.3)
        waiter = self.start(["python3", "-c", "import sys; sys.exit(0)"], slots="1",
                            env={"PLX_CHECK_LOCK": "off"})
        start = time.monotonic()
        rc, err = waiter.finish(timeout=5)
        self.assertEqual(rc, 0, err)
        self.assertLess(time.monotonic() - start, 2, "waiter blocked under PLX_CHECK_LOCK=off: " + err)
        holder.finish(timeout=5)
        self.assertFalse(self.lock_path.exists(), "PLX_CHECK_LOCK=off must not even create the lock")


class ExclusiveTests(LockTestCase):
    def test_an_exclusive_holder_blocks_shared_runs_and_is_named_as_exclusive(self):
        boss = self.hold(exclusive=True)
        err = self.assert_blocked({}, boss)
        self.assertIn("[exclusive]", err)

    def test_a_shared_holder_blocks_an_exclusive_run(self):
        shared = self.hold()
        err = self.assert_blocked({"exclusive": True}, shared)
        self.assertNotIn("[exclusive]", err)

    def test_an_exclusive_run_waits_for_every_shared_holder(self):
        a, b = self.hold(), self.hold()
        waiter = self.start(["python3", "-c", "import sys; sys.exit(0)"], exclusive=True)
        waiter.wait_for("waiting on")
        a.proc.kill()
        a.proc.wait()
        time.sleep(0.4)
        self.assertIsNone(waiter.proc.poll(), "exclusive run started with a shared run still alive")
        b.proc.kill()
        rc, err = waiter.finish()
        self.assertEqual(rc, 0, err)
        self.assertIn("exclusive: all 2 slots", err)

    def test_exclusive_covers_slots_a_wider_run_created(self):
        # A run started with PLX_CHECK_SLOTS=2 holds slot 1; an exclusive run started with
        # PLX_CHECK_SLOTS=1 must still wait for it.
        wide = [self.hold(slots="2"), self.hold(slots="2")]
        wide[0].proc.kill()
        wide[0].proc.wait()
        err = self.assert_blocked({"exclusive": True, "slots": "1"}, wide[1])
        self.assertIn("timed out", err)

    def test_a_killed_exclusive_holder_releases_the_machine(self):
        boss = self.hold(exclusive=True)
        waiter = self.start(["python3", "-c", "import sys; sys.exit(0)"])
        waiter.wait_for("waiting on")
        boss.proc.kill()
        rc, err = waiter.finish()
        self.assertEqual(rc, 0, err)

    def test_exclusive_gets_in_while_shared_requests_keep_arriving(self):
        log = self.root / "log"
        shared_runs, stop = [], threading.Event()

        def stream():
            n = 0
            while not stop.is_set():
                run = Run(self.root, self.lock_path,
                          ["python3", "-c", LOGGED, str(log), "s%d" % n, "0.4", str(self.lock_path)])
                shared_runs.append(run)
                self.runs.append(run)
                n += 1
                time.sleep(0.15)

        feeder = threading.Thread(target=stream, daemon=True)
        feeder.start()
        try:
            time.sleep(0.6)  # the stream is running, both slots are busy, a queue has formed
            asked_at = time.time()
            boss = self.logged(log, "boss", 0.3, exclusive=True, timeout=30)
            # The gate file names the pid: from here on the exclusive request is pending.
            pending_since = self.wait_for_gate(boss)
            rc, err = boss.finish(timeout=20)
            finished_at = time.time()
            self.assertEqual(rc, 0, err)
            # It was not starved: it got in within a few shared-run lengths of asking, while the
            # stream was still feeding new requests.
            self.assertLess(finished_at - asked_at, 5, err)
            self.assertTrue(feeder.is_alive())
            time.sleep(0.5)
        finally:
            stop.set()
            feeder.join(5)
        for run in shared_runs:
            rc, err = run.finish(timeout=30)
            self.assertEqual(rc, 0, err)
        times = read_log(log)
        boss_start, boss_end = times["boss"]["start"], times["boss"]["end"]
        for tag, span in times.items():
            if tag == "boss":
                continue
            overlaps = span["start"] < boss_end and span["end"] > boss_start
            self.assertFalse(overlaps, "%s overlapped the exclusive run: %r vs %r" % (tag, span, times["boss"]))
            if span["admit"] > pending_since:
                self.assertGreaterEqual(
                    span["admit"], boss_end,
                    "%s was admitted after the exclusive request was pending but before it finished" % tag)
        late = [t for t, span in times.items() if t != "boss" and span["start"] > boss_end]
        self.assertTrue(late, "the queued shared runs never ran after the exclusive one")

    def test_two_exclusive_requests_do_not_deadlock(self):
        log = self.root / "log"
        for round_no in range(3):
            shared = self.logged(log, "shared%d" % round_no, 0.8)
            shared.wait_for("acquired")
            bosses = [self.logged(log, "boss%d-%d" % (round_no, i), 0.2, exclusive=True, timeout=30)
                      for i in range(2)]
            for run in [shared, *bosses]:
                rc, err = run.finish(timeout=25)
                self.assertEqual(rc, 0, err)
        times = read_log(log)
        spans = sorted(times.items(), key=lambda item: item[1]["start"])
        for round_no in range(3):
            exclusive = [times["boss%d-%d" % (round_no, i)] for i in range(2)]
            first, second = sorted(exclusive, key=lambda s: s["start"])
            self.assertLessEqual(first["end"], second["start"], "two exclusive runs overlapped")
            shared = times["shared%d" % round_no]
            for span in exclusive:
                self.assertTrue(span["start"] >= shared["end"] or span["end"] <= shared["start"],
                                "an exclusive run overlapped a shared one")
        self.assertEqual(len(spans), 9)


class DefaultSlotTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("check_lock_under_test", SCRIPT)
        cls.mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.mod)

    def default(self, cpus, ram_gib, page=4096):
        sysconf = {"SC_PAGE_SIZE": page, "SC_PHYS_PAGES": ram_gib * 2**30 // page}
        with mock.patch.object(self.mod.os, "cpu_count", return_value=cpus), \
                mock.patch.object(self.mod.os, "sysconf", side_effect=lambda name: sysconf[name]):
            return self.mod.default_slots()

    def test_the_measured_machine_gets_two(self):
        self.assertEqual(self.default(10, 16), 2)

    def test_a_16_gb_linux_box_that_reports_slightly_less_still_gets_two(self):
        self.assertEqual(self.default(8, 15), 2)

    def test_fewer_cores_or_less_memory_gets_one(self):
        self.assertEqual(self.default(4, 16), 1)
        self.assertEqual(self.default(10, 8), 1)

    def test_a_bigger_machine_is_still_capped_at_the_measured_two(self):
        self.assertEqual(self.default(24, 128), 2)

    def test_unreadable_memory_gets_one(self):
        with mock.patch.object(self.mod.os, "sysconf", side_effect=ValueError):
            self.assertEqual(self.mod.default_slots(), 1)

    def test_the_environment_overrides_the_default(self):
        with mock.patch.dict(self.mod.os.environ, {"PLX_CHECK_SLOTS": "1"}):
            self.assertEqual(self.mod.slot_count(), 1)
        with mock.patch.dict(self.mod.os.environ, {"PLX_CHECK_SLOTS": "3"}):
            self.assertEqual(self.mod.slot_count(), 3)


if __name__ == "__main__":
    unittest.main()
