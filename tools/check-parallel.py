#!/usr/bin/env python3
"""Run a few independent `make check` branches at once, with deterministic output.

    tools/check-parallel.py [--jobs N] NAME=COMMAND [NAME=COMMAND ...]

`make check` used to be one serial chain, so the Python and shell gates (which never touch
`cargo` or `target/`) waited behind a quarter-hour of compiling, and the compiler sat idle behind
them. Each NAME=COMMAND is a branch; this runs them concurrently and keeps their output apart:

* A branch's stdout and stderr go to a private temp file, never to the terminal while it runs, so
  two branches cannot interleave a line.
* When every branch has passed, each branch's output is printed whole, in the order the branches
  were NAMED on the command line (not the order they finished), under a one-line banner.
* When a branch fails, the others are stopped (a failed gate has nothing to learn from a longer
  wait, and the serial chain would have stopped there too), the failing branch's whole output is
  printed, the stopped ones get one line each, and the exit status is non-zero: the first failing
  branch in command-line order, as that branch's own exit code, or 1.
* `--jobs N` caps how many run at once (default 2). `make check` runs on a Mac whose swap is
  routinely full, and every extra branch is another compiler or interpreter resident.

* SIGINT, SIGTERM, SIGHUP and SIGQUIT stop every branch's whole process group, wait for it, print
  what each branch had buffered so far under a "partial" banner, and exit with 128 + the signal.

A heartbeat on stderr every two minutes says which branches are still running; nothing else is
written while they run.
"""
from __future__ import annotations

import argparse
import os
import shlex
import signal
import subprocess
import sys
import tempfile
import time

HEARTBEAT = 120.0


class Branch:
    def __init__(self, name: str, command: str):
        self.name = name
        self.argv = shlex.split(command)
        self.log = tempfile.TemporaryFile(mode="w+b")
        self.proc: subprocess.Popen | None = None
        self.code: int | None = None  # set only for a branch that could not be started at all
        self.started = 0.0
        self.elapsed = 0.0
        self.stopped = False  # killed by us because a sibling failed

    def start(self) -> None:
        self.started = time.monotonic()
        # Own process group, so stopping the branch reaches the compiler and test binaries under it.
        try:
            self.proc = subprocess.Popen(self.argv, stdout=self.log, stderr=subprocess.STDOUT,
                                         stdin=subprocess.DEVNULL, start_new_session=True)
        except OSError as error:
            self.log.write(f"check-parallel: cannot start {self.argv[0]}: {error}\n".encode())
            self.code = 127

    def poll(self) -> int | None:
        code = self.code if self.proc is None else self.proc.poll()
        if code is not None and not self.elapsed:
            self.elapsed = time.monotonic() - self.started
        return code

    def stop(self) -> None:
        if self.proc is not None and self.proc.poll() is None:
            self.stopped = True
            for sig in (signal.SIGTERM, signal.SIGKILL):
                try:
                    os.killpg(self.proc.pid, sig)
                except ProcessLookupError:
                    break
                try:
                    self.proc.wait(timeout=10)
                    break
                except subprocess.TimeoutExpired:
                    continue
        self.elapsed = self.elapsed or time.monotonic() - self.started

    def text(self) -> str:
        self.log.flush()
        self.log.seek(0)
        return self.log.read().decode("utf-8", "replace")


def parse(items: list[str]) -> list[Branch]:
    branches = []
    for item in items:
        name, sep, command = item.partition("=")
        if not sep or not name or not command.strip():
            sys.exit(f"check-parallel: expected NAME=COMMAND, got {item!r}")
        branches.append(Branch(name, command))
    if len({b.name for b in branches}) != len(branches):
        sys.exit("check-parallel: branch names must be unique")
    return branches


def banner(branch: Branch, verdict: str) -> str:
    return f"==== check: {branch.name} — {verdict} ({branch.elapsed:.0f}s) ===="


class Interrupted(KeyboardInterrupt):
    """The runner was told to stop by a signal other than SIGINT; `signum` sets the exit status."""

    def __init__(self, signum: int):
        super().__init__(signum)
        self.signum = signum


def report_interrupted(branches: list[Branch], codes: dict[str, int]) -> None:
    """Print what each branch had buffered, in command-line order, marked as partial.

    An interrupted run (Ctrl-C, a tool timeout, a closed terminal) would otherwise show nothing at
    all, after possibly ten minutes of work. The terminal may be gone (SIGHUP), so a failed write is
    not an error here."""
    try:
        for branch in branches:
            if branch.proc is None and branch.code is None:
                print(banner(branch, "not started"))
            elif branch.name in codes:
                print(banner(branch, "ok" if codes[branch.name] == 0 else f"FAILED, exit {codes[branch.name]}"))
                sys.stdout.write(branch.text())
            else:
                print(banner(branch, "interrupted, output so far is partial"))
                sys.stdout.write(branch.text())
        sys.stdout.flush()
    except OSError:
        pass


def run(branches: list[Branch], jobs: int) -> int:
    pending = list(branches)
    running: list[Branch] = []
    codes: dict[str, int] = {}
    failed = False
    last_beat = time.monotonic()
    try:
        while pending or running:
            while pending and len(running) < jobs and not failed:
                branch = pending.pop(0)
                branch.start()
                running.append(branch)
            for branch in list(running):
                code = branch.poll()
                if code is not None:
                    running.remove(branch)
                    codes[branch.name] = code
                    failed = failed or code != 0
            if failed:
                for branch in running:
                    branch.stop()
                break
            if time.monotonic() - last_beat >= HEARTBEAT:
                last_beat = time.monotonic()
                print("check: still running: " + ", ".join(
                    f"{b.name} ({time.monotonic() - b.started:.0f}s)" for b in running),
                    file=sys.stderr, flush=True)
            time.sleep(0.2)
    except BaseException as error:
        for branch in running:
            branch.stop()
        if isinstance(error, KeyboardInterrupt):
            report_interrupted(branches, codes)
        raise

    if failed:
        # A sibling that exited between the last poll and stop() was never stopped; it has a verdict.
        for branch in running:
            code = branch.poll()
            if not branch.stopped and code is not None:
                codes[branch.name] = code
    if not failed:
        for branch in branches:
            print(banner(branch, "ok"))
            sys.stdout.write(branch.text())
            sys.stdout.flush()
        return 0
    first = next(b for b in branches if codes.get(b.name, 0) != 0 and not b.stopped)
    print(banner(first, f"FAILED, exit {codes[first.name]}"))
    sys.stdout.write(first.text())
    for branch in branches:
        if branch is first:
            continue
        if branch.stopped or branch.name not in codes:
            print(banner(branch, "stopped, a sibling branch failed" if branch.stopped else "not started"))
        elif codes[branch.name] != 0:
            print(banner(branch, f"ALSO FAILED, exit {codes[branch.name]}; its output follows"))
            sys.stdout.write(branch.text())
        else:
            print(banner(branch, "ok"))
    sys.stdout.flush()
    return codes[first.name] or 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--jobs", type=int, default=2, help="branches to run at once (default 2)")
    parser.add_argument("branch", nargs="+", metavar="NAME=COMMAND")
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be at least 1")
    return run(parse(args.branch), args.jobs)


def raise_interrupted(signum, _frame):
    raise Interrupted(signum)


if __name__ == "__main__":
    # Every branch is its own session, so none of these reaches the compiler on its own: SIGHUP (a
    # closed terminal, ssh, tmux) and SIGQUIT must stop the branches exactly as SIGTERM does, or they
    # outlive the runner while check-lock releases its machine-wide slot under them.
    for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGQUIT):
        signal.signal(sig, raise_interrupted)
    try:
        sys.exit(main())
    except Interrupted as error:
        sys.exit(128 + error.signum)
    except KeyboardInterrupt:
        sys.exit(130)
