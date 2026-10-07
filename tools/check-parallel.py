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

STEP MODE: `tools/check-parallel.py [--jobs N|auto] --steps FILE` runs a MANIFEST of independent
commands (one per line, `#` comments and blank lines ignored, each line is one command split like
a shell word list, no pipes or `&&`) instead of a few long branches. It is how `make
check-python-rest` runs its ~65 Python/shell/C gates (ci/check-python-steps.txt), for a lone
`make check` and for the CI job alike. It differs from branch mode in two ways, both because a step
is short and there are many of them:

* Each step's output is printed whole, under a one-line banner with its name, status, wall time and
  CPU time, the moment the step ends (still never interleaved: the step wrote to a private file).
* A failing step does NOT stop the others. Every step runs, so one run lists every red gate rather
  than the first (the old serial recipe stopped at the first, and the next run found the second).
  The run ends with a summary naming each failed step, and exits with the first failing step's own
  code in manifest order (or 1). A failure is therefore never lost in the scrollback of the green
  ones. (Branch mode keeps its fail-fast: a branch is minutes long and the serial chain it replaced
  stopped at the first failure.)

`--jobs auto` (the default in step mode) is `PLX_CHECK_STEP_JOBS` when set, otherwise `auto_jobs()`
below: the cores, bounded, because `make check` runs beside cargo and a second `make check`.
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
STEP_JOBS_CAP = 4  # lone `make check` at 2/3/4/6 steps: python branch 123/99-124/97/109 s, cargo unaffected at 4; see docs/agent-reference.md


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
        self.cpu: float | None = None  # user+system seconds of the whole process tree, once reaped

    def start(self) -> None:
        self.started = time.monotonic()
        # Own process group, so stopping the branch reaches the compiler and test binaries under it.
        try:
            self.proc = subprocess.Popen(self.argv, stdout=self.log, stderr=subprocess.STDOUT,
                                         stdin=subprocess.DEVNULL, start_new_session=True)
        except OSError as error:
            self.log.write(f"check-parallel: cannot start {self.argv[0]}: {error}\n".encode())
            self.code = 127

    def reap(self) -> None:
        """Collect the exit status with `wait4`, which also returns the process tree's resource use
        (a child's rusage includes every descendant it waited for). `Popen.poll` would discard it."""
        if self.proc is None or self.proc.returncode is not None:
            return
        try:
            pid, status, usage = os.wait4(self.proc.pid, os.WNOHANG)
        except ChildProcessError:
            return  # already reaped elsewhere; Popen.poll() below reports it
        if pid:
            self.proc.returncode = os.waitstatus_to_exitcode(status)
            self.cpu = usage.ru_utime + usage.ru_stime

    def poll(self) -> int | None:
        self.reap()
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


def read_steps(path: str) -> list[str]:
    """The commands of a step manifest, in file order: every line that is not blank or a `#` comment."""
    with open(path, encoding="utf-8") as handle:
        return [line.strip() for line in handle if line.strip() and not line.lstrip().startswith("#")]


def auto_jobs(cpus: int | None = None, override: str | None = None) -> int:
    """How many steps run at once when `--jobs` is not a number.

    `PLX_CHECK_STEP_JOBS` wins. Otherwise the core count, between 2 and STEP_JOBS_CAP: the steps are
    mostly short interpreters, so cores are the limit on a CI runner (4), while on a developer Mac
    `make check` runs this beside cargo's own `-Zthreads=8` builds and a second `make check`, and more
    than the cap only adds resident interpreters and timing noise for the lock and parallel tests."""
    if override:
        try:
            return max(1, int(override))
        except ValueError:
            sys.exit(f"check-parallel: PLX_CHECK_STEP_JOBS must be a number, got {override!r}")
    return max(2, min(cpus if cpus is not None else (os.cpu_count() or 2), STEP_JOBS_CAP))


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


def step_banner(step: Branch, verdict: str) -> str:
    cpu = f", cpu {step.cpu:.1f}s" if step.cpu is not None else ""
    return f"==== step: {step.name} — {verdict} ({step.elapsed:.1f}s{cpu}) ===="


def print_step(step: Branch, verdict: str) -> None:
    """One step's banner and whole output, as a single burst: the step ran against a private file, and
    only this thread prints, so two steps' lines can never interleave."""
    text = step.text()
    sys.stdout.write(step_banner(step, verdict) + "\n" + text + ("" if not text or text.endswith("\n") else "\n"))
    sys.stdout.flush()


def run_steps(steps: list[Branch], jobs: int) -> int:
    """Run every step, at most `jobs` at once, printing each when it ends; none is skipped when one fails."""
    pending = list(steps)
    running: list[Branch] = []
    codes: dict[Branch, int] = {}
    began = last_beat = time.monotonic()
    try:
        while pending or running:
            while pending and len(running) < jobs:
                step = pending.pop(0)
                step.start()
                running.append(step)
            for step in list(running):
                code = step.poll()
                if code is not None:
                    running.remove(step)
                    codes[step] = code
                    print_step(step, "ok" if code == 0 else f"FAILED, exit {code}")
            if time.monotonic() - last_beat >= HEARTBEAT:
                last_beat = time.monotonic()
                print("check: still running: " + ", ".join(
                    f"{s.name} ({time.monotonic() - s.started:.0f}s)" for s in running),
                    file=sys.stderr, flush=True)
            time.sleep(0.05)
    except BaseException as error:
        for step in running:
            step.stop()
        if isinstance(error, KeyboardInterrupt):
            try:
                for step in running:
                    print_step(step, "interrupted, output so far is partial")
                print(f"==== check: interrupted — {len(codes)} of {len(steps)} steps had finished ====", flush=True)
            except OSError:
                pass  # the terminal may be gone (SIGHUP)
        raise

    wall = time.monotonic() - began
    failed = [s for s in steps if codes[s] != 0]
    cpu = sum(s.cpu or 0.0 for s in steps)
    slowest = ", ".join(f"{s.name.split('/')[-1]} {s.elapsed:.0f}s"
                        for s in sorted(steps, key=lambda s: -s.elapsed)[:3])
    if not failed:
        print(f"==== check: {len(steps)} steps — all ok ({wall:.0f}s wall, {cpu:.0f}s cpu, {jobs} at once; slowest: {slowest}) ====",
              flush=True)
        return 0
    print(f"==== check: {len(failed)} of {len(steps)} steps FAILED ({wall:.0f}s wall, {jobs} at once) ====")
    for step in failed:
        print(f"FAILED (exit {codes[step]}): {step.name}")
    sys.stdout.flush()
    first = codes[failed[0]]
    return first if first > 0 else 128 - first if first < 0 else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--jobs", default=None,
                        help="how many to run at once: a number, or `auto` (step mode's default). "
                             "Default 2 for branches")
    parser.add_argument("--steps", metavar="FILE", help="run the commands of this manifest (step mode)")
    parser.add_argument("branch", nargs="*", metavar="NAME=COMMAND")
    args = parser.parse_args()
    if args.steps and args.branch:
        parser.error("--steps and NAME=COMMAND branches are different modes; give one")
    if not args.steps and not args.branch:
        parser.error("give --steps FILE or at least one NAME=COMMAND")
    if args.jobs in (None, "auto"):
        jobs = auto_jobs(override=os.environ.get("PLX_CHECK_STEP_JOBS")) if args.steps else 2
    else:
        try:
            jobs = int(args.jobs)
        except ValueError:
            parser.error("--jobs must be a number or `auto`")
    if jobs < 1:
        parser.error("--jobs must be at least 1")
    if args.steps:
        commands = read_steps(args.steps)
        if not commands:
            sys.exit(f"check-parallel: {args.steps} lists no steps")
        if len(set(commands)) != len(commands):
            sys.exit(f"check-parallel: {args.steps} lists a step twice: "
                     f"{sorted({c for c in commands if commands.count(c) > 1})}")
        return run_steps([Branch(command, command) for command in commands], jobs)
    return run(parse(args.branch), jobs)


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
