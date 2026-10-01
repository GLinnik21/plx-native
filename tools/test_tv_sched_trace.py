#!/usr/bin/env python3
"""Grade tools/tv-sched-trace.sh's read-back against a fake tracefs and a fake tv-ssh.

The fake tv-ssh runs the "remote" command locally with sh (as ssh hands it to the set's shell); the
fake tracefs is a directory whose `trace_pipe` is a FIFO fed by a writer that emits N lines and
then blocks, the way the real pipe blocks when the ring is empty. Covered: (a) the drained trace is
a valid gzip holding every line and the script exits 0; (b) --read-timeout against a ring that
never drains yields a valid PARTIAL gzip, the partial notice, exit status 3; (c) in both, restore
ran: events off, filters cleared, buffer/clock/tracing_on put back, no reader left behind. Nothing
here touches a television.
"""
import gzip
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest

SCRIPT = Path(__file__).with_name("tv-sched-trace.sh")
FAKE_SSH = '#!/bin/sh\nshift 2\nexec sh -c "$*"\n'
LINES = [f"  app-100 [00{i % 2}] .... {1.0 + i / 1000:.6f}: sched_switch: prev_comm=app prev_pid=100 prev_prio=120 prev_state=S ==> next_comm=sw next_pid=0 next_prio=120"
         for i in range(200)]


def write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


class Rig:
    def __init__(self, root, entries):
        self.root = root
        self.t = root / "tracing"
        self.pidf = root / "pids"
        write(self.t / "trace_clock", "[local] global counter mono\n")
        write(self.t / "buffer_size_kb", "1408\n")
        write(self.t / "tracing_on", "1\n")
        write(self.t / "set_event", "")
        write(self.t / "trace", "")
        for cpu in (0, 1):
            write(self.t / f"per_cpu/cpu{cpu}/stats", f"entries: {entries}\noverrun: {7 if cpu == 0 else 0}\n")
        for ev in ("sched/sched_switch", "sched/sched_wakeup", "irq/irq_handler_entry",
                   "irq/irq_handler_exit", "raw_syscalls/sys_enter", "raw_syscalls/sys_exit"):
            write(self.t / f"events/{ev}/enable", "0\n")
            write(self.t / f"events/{ev}/filter", "none\n")
        ssh = root / "tv-ssh"
        write(ssh, FAKE_SSH)
        ssh.chmod(0o755)
        os.mkfifo(self.t / "trace_pipe")
        # The writer holds the pipe open after N lines, so a reader blocks like on an empty ring.
        self.writer = subprocess.Popen(
            ["sh", "-c", 'cat > "$1"; sleep 60', "sh", str(self.t / "trace_pipe")],
            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            text=True, start_new_session=True)
        self.writer.stdin.write("\n".join(LINES) + "\n")
        self.writer.stdin.flush()

    def run(self, *args, timeout=60):
        env = dict(os.environ, TV_SCHED_TRACE_TEST="1", TV_SCHED_TRACE_TVSSH=str(self.root / "tv-ssh"),
                   TV_SCHED_TRACE_T=str(self.t), TV_SCHED_TRACE_PIDF=str(self.pidf))
        return subprocess.run(["bash", str(SCRIPT), *args, "--out", str(self.root / "out.gz")],
                              env=env, capture_output=True, text=True, timeout=timeout)

    def close(self):
        os.killpg(self.writer.pid, signal.SIGKILL)
        self.writer.wait()
        self.writer.stdin.close()

    def read(self, rel):
        return (self.t / rel).read_text().strip()

    def assert_restored(self, case):
        case.assertEqual(self.read("tracing_on"), "1")
        case.assertEqual(self.read("trace_clock"), "[local] global counter mono".split()[0].strip("[]"))
        case.assertEqual(self.read("buffer_size_kb"), "1408")
        for ev in ("sched/sched_switch", "irq/irq_handler_entry", "raw_syscalls/sys_enter",
                   "raw_syscalls/sys_exit"):
            case.assertEqual(self.read(f"events/{ev}/enable"), "0", ev)
        for ev in ("irq/irq_handler_entry", "irq/irq_handler_exit", "raw_syscalls/sys_enter"):
            case.assertEqual(self.read(f"events/{ev}/filter"), "0", ev)
        case.assertFalse(self.pidf.exists(), "reader pid file left behind")
        left = subprocess.run(["pgrep", "-f", str(self.t / "trace_pipe")], capture_output=True, text=True)
        # The test's own writer holds the fifo via `cat >`; a leftover READER is a `cat <path>` without `>`.
        readers = [p for p in left.stdout.split() if "cat" in subprocess.run(
            ["ps", "-o", "command=", "-p", p], capture_output=True, text=True).stdout and
            ">" not in subprocess.run(["ps", "-o", "command=", "-p", p], capture_output=True, text=True).stdout]
        case.assertEqual(readers, [], "a trace_pipe reader is still running")


class ReadBack(unittest.TestCase):
    def rig(self, entries):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        rig = Rig(Path(d.name), entries)
        self.addCleanup(rig.close)
        return rig

    def test_drain_is_complete_and_restores(self):
        rig = self.rig(0)
        r = rig.run("--secs", "1", "--tid", "100")
        self.assertEqual(r.returncode, 0, r.stderr)
        with gzip.open(rig.root / "out.gz", "rt") as f:
            self.assertEqual(f.read().splitlines(), LINES)
        self.assertIn("overwritten", r.stderr)
        self.assertIn("OVERRAN", r.stderr)
        self.assertNotIn("PARTIAL", r.stderr)
        rig.assert_restored(self)

    def test_read_timeout_keeps_a_valid_partial_gzip(self):
        rig = self.rig(5)   # the ring never reads as empty, so only the host's bound ends it
        started = time.time()
        r = rig.run("--secs", "1", "--read-timeout", "2")
        self.assertEqual(r.returncode, 3, r.stderr)
        self.assertLess(time.time() - started, 30)
        self.assertIn("PARTIAL trace", r.stderr)
        with gzip.open(rig.root / "out.gz", "rt") as f:   # raises on a truncated stream
            got = f.read().splitlines()
        self.assertGreater(len(got), 100)
        self.assertGreater(len(got), 150)
        self.assertEqual(got, LINES[:len(got)])
        rig.assert_restored(self)

    def test_interrupt_during_read_back_leaves_a_valid_gzip(self):
        rig = self.rig(5)
        env = dict(os.environ, TV_SCHED_TRACE_TEST="1", TV_SCHED_TRACE_TVSSH=str(rig.root / "tv-ssh"),
                   TV_SCHED_TRACE_T=str(rig.t), TV_SCHED_TRACE_PIDF=str(rig.pidf))
        p = subprocess.Popen(["bash", str(SCRIPT), "--secs", "1", "--out", str(rig.root / "out.gz")],
                             env=env, stderr=subprocess.PIPE, text=True)
        deadline = time.time() + 30
        while time.time() < deadline and not rig.pidf.exists():
            time.sleep(0.2)
        time.sleep(1)
        p.send_signal(signal.SIGTERM)  # same trap as ^C; SIGINT is ignored when the harness itself runs in the background
        _, err = p.communicate(timeout=30)
        self.assertEqual(p.returncode, 130, err)
        with gzip.open(rig.root / "out.gz", "rt") as f:
            self.assertGreater(len(f.read().splitlines()), 100)
        rig.assert_restored(self)

    def test_bad_arguments(self):
        rig = self.rig(0)
        self.assertEqual(rig.run("--tid", "x").returncode, 2)
        self.assertEqual(rig.run("--read-timeout", "0").returncode, 2)


if __name__ == "__main__":
    unittest.main()
