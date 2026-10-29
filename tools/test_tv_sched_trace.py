#!/usr/bin/env python3
"""Grade tools/tv-sched-trace.sh's read-back against a fake tracefs and a fake tv-ssh.

The fake tv-ssh runs the "remote" command locally with sh (as ssh hands it to the set's shell); the
fake tracefs is a directory whose `trace_pipe` is a FIFO fed by a writer that emits N lines and
then blocks, the way the real pipe blocks when the ring is empty. Covered: (a) the drained trace is
a valid gzip holding every line and the script exits 0; (b) --read-timeout against a ring that
never drains yields a valid PARTIAL gzip, the partial notice, exit status 3; (c) in both, restore
ran: events off, filters cleared, buffer/clock/tracing_on put back, no reader left behind; (d) the
thread table (`# plx-task` lines for the traced process and every surface-manager process) heads
the same gzip; (e) `--app` finds the one process whose exe ENDS in the install's binary path (a
jail prefixes it), and refuses, before touching the set, on none or on two. Nothing here touches a
television: the "set" is a directory tree that stands in for /proc.
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


CLOCK = "local"   # the trace_clock the fake set starts on, and what the script must restore
APPDIR = "/media/developer/apps/usr/palm/applications/com.beb.plxnative.debug"
STABLE_DIR = "/media/developer/apps/usr/palm/applications/com.beb.plxnative"
JAIL = "/var/palm/jail/com.beb.plxnative.debug"
# The fake /proc: pid -> (exe link target or None, main-thread comm, {tid: comm}). Pid 100 is the debug
# app (jailed, so its exe has the jail prefix), 60 the STABLE app (its path is a prefix of the debug
# one's directory name, not a suffix match), 50 the compositor, 70 an unrelated process.
PROCS = {
    100: (f"{JAIL}{APPDIR}/plxnative", "plxnative", {100: "plxnative", 101: "mali-event-hnd", 102: "mali-renderer"}),
    60: (f"/var/palm/jail/com.beb.plxnative{STABLE_DIR}/plxnative", "plxnative", {60: "plxnative", 61: "mali-event-hnd"}),
    50: ("/usr/sbin/surface-manager", "surface-manager", {50: "surface-manager", 51: "mali-event-hnd"}),
    70: ("/bin/busybox", "sh", {70: "sh"}),
}
TABLE_100 = ["# plx-task tgid=100 tid=100 comm=plxnative", "# plx-task tgid=100 tid=101 comm=mali-event-hnd",
             "# plx-task tgid=100 tid=102 comm=mali-renderer"]
TABLE_SM = ["# plx-task tgid=50 tid=50 comm=surface-manager", "# plx-task tgid=50 tid=51 comm=mali-event-hnd"]


def fake_proc(root, procs):
    """Write the /proc stand-in: `exe` symlinks (dangling, like the jail's), comm files, task dirs."""
    for pid, (exe, comm, tasks) in procs.items():
        d = root / str(pid)
        d.mkdir(parents=True, exist_ok=True)
        if exe:
            os.symlink(exe, d / "exe")
        write(d / "comm", comm + "\n")
        write(d / "status", f"Name:\t{comm}\nTgid:\t{pid}\nPid:\t{pid}\n")
        for tid, tcomm in tasks.items():
            write(d / "task" / str(tid) / "comm", tcomm + "\n")


class Rig:
    def __init__(self, root, entries):
        self.root = root
        self.t = root / "tracing"
        self.pidf = root / "pids"
        write(self.t / "trace_clock", f"[{CLOCK}] global counter mono\n")
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
        self.proc = root / "proc"
        fake_proc(self.proc, PROCS)
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

    def env(self):
        return dict(os.environ, TV_SCHED_TRACE_TEST="1", TV_SCHED_TRACE_TVSSH=str(self.root / "tv-ssh"),
                    TV_SCHED_TRACE_T=str(self.t), TV_SCHED_TRACE_PIDF=str(self.pidf),
                    TV_SCHED_TRACE_PROC=str(self.proc), TV_SCHED_TRACE_APPDIR=APPDIR)

    def run(self, *args, timeout=60, env=None):
        return subprocess.run(["bash", str(SCRIPT), *args, "--out", str(self.root / "out.gz")],
                              env=dict(self.env(), **(env or {})), capture_output=True, text=True,
                              timeout=timeout)

    def trace(self):
        """(thread-table lines, event lines) of the gzip the last run wrote."""
        with gzip.open(self.root / "out.gz", "rt") as f:   # raises on a truncated stream
            got = f.read().splitlines()
        table = [ln for ln in got if ln.startswith("# plx-task ")]
        assert got[:len(table)] == table, "the thread table must head the stream"
        return table, got[len(table):]

    def close(self):
        os.killpg(self.writer.pid, signal.SIGKILL)
        self.writer.wait()
        self.writer.stdin.close()

    def read(self, rel):
        return (self.t / rel).read_text().strip()

    def assert_restored(self, case):
        case.assertEqual(self.read("tracing_on"), "1")
        case.assertEqual(self.read("trace_clock"), CLOCK)
        case.assertEqual(self.read("buffer_size_kb"), "1408")
        for ev in ("sched/sched_switch", "irq/irq_handler_entry", "raw_syscalls/sys_enter",
                   "raw_syscalls/sys_exit"):
            case.assertEqual(self.read(f"events/{ev}/enable"), "0", ev)
        for ev in ("irq/irq_handler_entry", "irq/irq_handler_exit", "raw_syscalls/sys_enter"):
            case.assertEqual(self.read(f"events/{ev}/filter"), "0", ev)
        case.assertFalse(self.pidf.exists(), "reader pid file left behind")
        left = subprocess.run(["pgrep", "-f", str(self.t / "trace_pipe")], capture_output=True, text=True)
        # The test's own writer holds the fifo via `cat >`; a leftover READER is a `cat <path>` without `>`.
        readers = []
        for p in left.stdout.split():
            cmd = subprocess.run(["ps", "-o", "command=", "-p", p], capture_output=True, text=True).stdout
            if "cat" in cmd and ">" not in cmd:
                readers.append(p)
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
        table, events = rig.trace()
        self.assertEqual(events, LINES)
        self.assertEqual(sorted(table), sorted(TABLE_100 + TABLE_SM))
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
        table, got = rig.trace()
        self.assertEqual(sorted(table), sorted(TABLE_SM))   # no --tid: only the compositor's threads
        self.assertGreater(len(got), 150)
        self.assertEqual(got, LINES[:len(got)])
        rig.assert_restored(self)

    def test_interrupt_during_read_back_leaves_a_valid_gzip(self):
        rig = self.rig(5)
        p = subprocess.Popen(["bash", str(SCRIPT), "--secs", "1", "--out", str(rig.root / "out.gz")],
                             env=rig.env(), stderr=subprocess.PIPE, text=True)
        deadline = time.time() + 30
        while time.time() < deadline and not rig.pidf.exists():
            time.sleep(0.2)
        time.sleep(1)
        p.send_signal(signal.SIGTERM)  # same trap as ^C; SIGINT is ignored when the harness itself runs in the background
        _, err = p.communicate(timeout=30)
        self.assertEqual(p.returncode, 130, err)
        self.assertGreater(len(rig.trace()[1]), 100)
        rig.assert_restored(self)

    def test_bad_arguments(self):
        rig = self.rig(0)
        self.assertEqual(rig.run("--tid", "x").returncode, 2)
        self.assertEqual(rig.run("--read-timeout", "0").returncode, 2)
        self.assertEqual(rig.run("--app=Debug").returncode, 2)

    def test_tid_and_app_are_exclusive(self):
        rig = self.rig(0)
        for args in (("--tid", "100", "--app"), ("--app", "debug", "--tid", "100"), ("--app=debug", "--tid=100")):
            r = rig.run(*args)
            self.assertEqual(r.returncode, 2, r.stderr)
            self.assertIn("mutually exclusive", r.stderr)
        self.assertFalse((rig.root / "out.gz").exists())

    def test_app_finds_the_jailed_exe_by_suffix(self):
        rig = self.rig(0)
        r = rig.run("--app", "--secs", "1")   # bare --app: the flavour is $FLAVOR, else debug
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("--app debug: pid 100", r.stderr)
        self.assertIn("syscalls of tid 100", r.stderr)
        table, events = rig.trace()
        self.assertEqual(events, LINES)
        # The traced process's own threads and the compositor's; not the stable app's (pid 60, whose
        # directory differs only by the `.debug` the debug install adds) or an unrelated process.
        self.assertEqual(sorted(table), sorted(TABLE_100 + TABLE_SM))
        rig.assert_restored(self)

    def test_app_takes_a_flavour_in_either_spelling(self):
        for args in (("--app", "debug", "--secs", "1"), ("--app=debug", "--secs", "1")):
            r = self.rig(0).run(*args)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("--app debug: pid 100", r.stderr)
        rig = self.rig(0)   # the stable install is another directory: pid 60, not the debug app
        r = rig.run("--app", "stable", "--secs", "1", env={"TV_SCHED_TRACE_APPDIR": STABLE_DIR})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("--app stable: pid 60", r.stderr)
        table, _ = rig.trace()
        self.assertIn("# plx-task tgid=60 tid=61 comm=mali-event-hnd", table)
        self.assertNotIn(TABLE_100[0], table)

    def test_app_matches_an_unjailed_exe_too(self):
        rig = self.rig(0)
        (rig.proc / "100" / "exe").unlink()
        os.symlink(f"{APPDIR}/plxnative", rig.proc / "100" / "exe")
        r = rig.run("--app", "--secs", "1")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("pid 100", r.stderr)

    def test_app_refuses_when_nothing_runs_it_and_leaves_the_set_alone(self):
        rig = self.rig(0)
        r = rig.run("--app", "--secs", "1", env={"TV_SCHED_TRACE_APPDIR": f"{APPDIR}-none"})
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertEqual(len(r.stderr.strip().splitlines()), 1, r.stderr)
        self.assertIn("no debug app is running", r.stderr)
        self.assertFalse((rig.root / "out.gz").exists())
        # It never reached setup: the clock file is still the fake set's own text (a restore would
        # have rewritten it), tracing is still on and nothing was enabled.
        self.assertEqual(rig.read("trace_clock"), f"[{CLOCK}] global counter mono")
        self.assertEqual(rig.read("tracing_on"), "1")
        self.assertEqual(rig.read("events/sched/sched_switch/enable"), "0")
        self.assertEqual(rig.read("events/irq/irq_handler_entry/filter"), "none")
        self.assertFalse(rig.pidf.exists())

    def test_app_names_a_process_that_still_runs_a_replaced_binary(self):
        rig = self.rig(0)
        (rig.proc / "100" / "exe").unlink()
        os.symlink(f"{JAIL}{APPDIR}/plxnative (deleted)", rig.proc / "100" / "exe")
        r = rig.run("--app", "--secs", "1")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertEqual(len(r.stderr.strip().splitlines()), 1, r.stderr)
        self.assertIn("pid 100 still runs a replaced binary", r.stderr)

    def test_app_refuses_two_matches(self):
        rig = self.rig(0)
        fake_proc(rig.proc, {90: (f"{JAIL}{APPDIR}/plxnative", "plxnative", {90: "plxnative"})})
        r = rig.run("--app", "--secs", "1")
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertEqual(len(r.stderr.strip().splitlines()), 1, r.stderr)
        self.assertIn("more than one process", r.stderr)
        self.assertIn("100", r.stderr)
        self.assertIn("90", r.stderr)
        self.assertFalse((rig.root / "out.gz").exists())
        r = rig.run("--secs", "1", "--tid", "90")   # --tid names the one to trace and goes through
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("# plx-task tgid=90 tid=90 comm=plxnative", rig.trace()[0])


if __name__ == "__main__":
    unittest.main()
