#!/usr/bin/env python3
"""Serialize `make check` machine-wide with a single flock, across every worktree.

Several agent worktrees running `make check` at once thrash one Mac: each is a cold
412k-line rustc build (~1 GB RSS), and on 2026-09-28 seven concurrent runs pushed swap
to 9-15 GB and stretched a ~10-minute run to 60 minutes. Queuing them is strictly
faster for everyone, and a `flock`-backed lock file does that without a daemon: the
kernel releases the lock the instant the holding process dies, so there is never a
stale lock to clean up by hand (unlike a mkdir/pidfile scheme).

Usage:
    check-lock.py [--lock PATH] [--timeout SECONDS] -- CMD...

The lock file defaults to `${PLX_CHECK_LOCK:-~/.cache/plxnative/check.lock}`, which is
deliberately OUTSIDE any git worktree so every checkout on the machine contends for the
same file. Set `PLX_CHECK_LOCK=off` to bypass locking entirely (escape hatch for a
machine known to be otherwise idle, or for debugging the wrapper itself).

While waiting, the current holder's identity (pid, worktree, start time) is printed
once immediately and then every 60 s, read out of the lock file's own contents: the
holder truncates and writes it (then fsyncs) right after acquiring, so a waiter never
prints stale information. With no `--timeout`, this blocks indefinitely; with one, it
exits 75 (EX_TEMPFAIL) if the lock is still held when the timeout elapses.

Once acquired, CMD runs as a child in its own process group so that whatever it spawns
can be signaled as a unit; SIGINT/SIGTERM/SIGHUP are forwarded to that group, and the
wrapper exits with the child's exit status (128+signal if the child was signaled). The
lock fd is opened by THIS process only — Python file descriptors default to
non-inheritable (O_CLOEXEC), and nothing here changes that — so a stray daemon the
child leaves running cannot pin the lock after the wrapper exits.
"""
import fcntl
import json
import os
import signal
import subprocess
import sys
import time

DEFAULT_LOCK = os.path.expanduser("~/.cache/plxnative/check.lock")
POLL_INTERVAL = 2
REPORT_INTERVAL = 60
EX_TEMPFAIL = 75


def default_lock_path():
    override = os.environ.get("PLX_CHECK_LOCK")
    if override:
        return override
    return DEFAULT_LOCK


def parse_args(argv):
    lock_path = None
    timeout = None
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--":
            i += 1
            break
        if arg == "--lock":
            i += 1
            if i >= len(argv):
                sys.exit("check-lock: --lock requires a path")
            lock_path = argv[i]
        elif arg.startswith("--lock="):
            lock_path = arg.split("=", 1)[1]
        elif arg == "--timeout":
            i += 1
            if i >= len(argv):
                sys.exit("check-lock: --timeout requires a number of seconds")
            timeout = float(argv[i])
        elif arg.startswith("--timeout="):
            timeout = float(arg.split("=", 1)[1])
        else:
            sys.exit("check-lock: unrecognized argument %r (commands go after --)" % (arg,))
        i += 1
    cmd = argv[i:]
    if not cmd:
        sys.exit("usage: check-lock.py [--lock PATH] [--timeout SECONDS] -- CMD...")
    if lock_path is None:
        lock_path = default_lock_path()
    return lock_path, timeout, cmd


def describe_holder(lock_path):
    """Best-effort read of the holder info the current lock owner wrote. May be stale
    by a few seconds, or missing entirely if the holder hasn't written it yet — never
    treated as authoritative, only as a courtesy to whoever is waiting."""
    try:
        with open(lock_path, "r") as fh:
            raw = fh.read().strip()
        if not raw:
            return None
        return json.loads(raw)
    except (OSError, ValueError):
        return None


def format_holder(info):
    if not info:
        return "check-lock: waiting on another `make check` (holder identity not available yet)"
    started = info.get("started", "?")
    return (
        "check-lock: waiting on pid %s in %s (started %s)"
        % (info.get("pid", "?"), info.get("worktree", "?"), started)
    )


def acquire(lock_path, timeout):
    lock_dir = os.path.dirname(lock_path)
    if lock_dir:
        os.makedirs(lock_dir, exist_ok=True)
    fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o644)
    deadline = None if timeout is None else time.monotonic() + timeout
    printed_first = False
    last_report = 0.0
    wait_start = time.monotonic()
    while True:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            break
        except BlockingIOError:
            pass
        now = time.monotonic()
        if not printed_first or now - last_report >= REPORT_INTERVAL:
            info = describe_holder(lock_path)
            print(format_holder(info), file=sys.stderr)
            printed_first = True
            last_report = now
        if deadline is not None and now >= deadline:
            info = describe_holder(lock_path)
            print(
                "check-lock: timed out after %.0fs waiting for the lock (%s)"
                % (timeout, format_holder(info)),
                file=sys.stderr,
            )
            os.close(fd)
            sys.exit(EX_TEMPFAIL)
        time.sleep(POLL_INTERVAL)
    elapsed = time.monotonic() - wait_start
    print("check-lock: acquired after %.0fs" % (elapsed,), file=sys.stderr)
    info = {
        "pid": os.getpid(),
        "worktree": os.getcwd(),
        "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    }
    os.ftruncate(fd, 0)
    os.lseek(fd, 0, os.SEEK_SET)
    os.write(fd, json.dumps(info).encode("utf-8"))
    os.fsync(fd)
    return fd


def run_locked(cmd):
    proc = subprocess.Popen(cmd, start_new_session=True)

    def forward(signum, _frame):
        try:
            os.killpg(proc.pid, signum)
        except ProcessLookupError:
            pass

    previous = {}
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        previous[sig] = signal.signal(sig, forward)
    try:
        proc.wait()
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
    if proc.returncode < 0:
        return 128 - proc.returncode
    return proc.returncode


def main(argv):
    lock_path, timeout, cmd = parse_args(argv)
    if os.environ.get("PLX_CHECK_LOCK") == "off":
        return run_locked(cmd)
    fd = acquire(lock_path, timeout)
    try:
        return run_locked(cmd)
    finally:
        # Closing the fd releases the flock; no explicit LOCK_UN needed, and this way a
        # crash between acquire() and here still releases it via process exit.
        os.close(fd)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
