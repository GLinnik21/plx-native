#!/usr/bin/env python3
"""Bound `make check` machine-wide to a few concurrent runs, across every worktree.

Several agent worktrees running `make check` at once thrash one Mac: each is a cold
412k-line rustc build (~1 GB RSS), and on 2026-09-28 seven concurrent runs pushed swap
to 9-15 GB and stretched a ~10-minute run to 60 minutes. Queuing them beyond a small
number is strictly faster for everyone, and `flock`-backed lock files do that without a
daemon: the kernel releases a lock the instant the holding process dies, so there is
never a stale lock to clean up by hand (unlike a mkdir/pidfile scheme).

Usage:
    check-lock.py [--lock PATH] [--timeout SECONDS] [--exclusive] -- CMD...

SLOTS. The lock is N slots, not one. A run takes any free slot (it tries each without
blocking, then polls every POLL_INTERVAL s) and holds it for CMD's lifetime. Slot 0 is the
lock file itself (`${PLX_CHECK_LOCK:-~/.cache/plxnative/check.lock}`) and slot i is
`<lock>.i`; keeping slot 0 at the historical path means an older single-lock wrapper still
running in some other worktree holds a slot of this scheme instead of being invisible to
it. The directory is deliberately OUTSIDE any git worktree so every checkout on the machine
contends for the same files. `PLX_CHECK_LOCK=off` bypasses locking entirely.

HOW MANY. `PLX_CHECK_SLOTS=N` (or the default below). N=1 is the old one-at-a-time lock
exactly. Measured 2026-10-06 on an Apple M4 (10 cores, 16 GB): a lone warm `make check` is
185-205 s; two at once finish in 0.67-0.76 of the back-to-back time (warm, cold, and cold
without the cargo seed cache), peak memory for the pair 5-6 GB with no swap growth, each run
of a pair 1.35-1.55x slower than alone. Three at once was NOT measured and is expected to
swap on 16 GB, so the default never exceeds 2. The default takes the measured machine as the
reference: 2 slots when the host has at least 8 cores and 14 GiB of RAM (the measured pair
left ~10 GB free; fewer cores starve both runs, less RAM swaps), otherwise 1, and 1 when the
memory cannot be read. A bigger machine is not given more than 2 because that was not
measured; raise it with PLX_CHECK_SLOTS at your own risk.

EXCLUSIVE. `--exclusive` (the benchmarks, `make build-bench*`) takes the whole machine: ALL
slots (the larger of N and any slot file that exists, so a run started with a different
PLX_CHECK_SLOTS is still excluded) plus the intent gate `<lock>.gate`. The gate is what makes
it fair and deadlock-free:
  * An exclusive request first takes the gate (LOCK_EX), then collects the slots one by one,
    keeping those it has while it waits for the others. A shared request takes the gate
    LOCK_SH, non-blocking, only for the instant it claims a slot; while an exclusive
    request holds the gate a shared request cannot even look at the slots. So once a
    benchmark has asked, no NEW check starts: it waits only for the at most N checks
    already running, never for later arrivals (no starvation of an exclusive request by a
    stream of checks).
  * Only one exclusive request at a time can hold the gate, and it is the only one
    collecting slots, so two benchmarks cannot each hold part of the slots and wait for
    the rest (no deadlock). The gate stays held until the benchmark's command exits, so a
    shared run cannot slip in between the slots and the command either.
  * Not guaranteed: an order between two exclusive requests (they poll, they are not a
    queue), and shared waiters can in theory keep losing to a continuous stream of
    benchmarks; benchmarks are rare and a person starts them.

While waiting, EVERY current holder's identity (pid, worktree, start time) is printed
once immediately and then every 60 s, read out of the lock files' own contents: a holder
writes its identity into its lock file(s) (and fsyncs) right after acquiring and empties
them on a normal exit, so a waiter does not print a finished run's identity. With no
`--timeout`, this blocks indefinitely; with one, it exits 75 (EX_TEMPFAIL) if no slot was
obtained when the timeout elapsed.

Once acquired, CMD runs as a child in its own process group so that whatever it spawns
can be signaled as a unit; SIGINT/SIGTERM/SIGHUP are forwarded to that group, and the
wrapper exits with the child's exit status (128+signal if the child was signaled). The
lock fds are opened by THIS process only -- Python file descriptors default to
non-inheritable (O_CLOEXEC), and nothing here changes that -- so a stray daemon the
child leaves running cannot pin a slot after the wrapper exits.

`PLX_CHECK_POLL=SECONDS` replaces POLL_INTERVAL; it exists so ci/test_check_lock.py can
exercise the waiting paths in a second or two, not for use.
"""
import fcntl
import glob
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
MAX_SLOTS = 8
# The measured machine (see the module docstring) is the reference for the default slot count.
MIN_CPUS_FOR_TWO = 8
MIN_RAM_FOR_TWO = 14 * 2**30


def default_lock_path():
    override = os.environ.get("PLX_CHECK_LOCK")
    if override:
        return override
    return DEFAULT_LOCK


def default_slots():
    try:
        ram = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
    except (ValueError, OSError, AttributeError):
        return 1
    if (os.cpu_count() or 1) >= MIN_CPUS_FOR_TWO and ram >= MIN_RAM_FOR_TWO:
        return 2
    return 1


def slot_count():
    raw = os.environ.get("PLX_CHECK_SLOTS")
    if not raw:
        return default_slots()
    try:
        count = int(raw)
    except ValueError:
        count = 0
    if not 1 <= count <= MAX_SLOTS:
        sys.exit(
            "check-lock: PLX_CHECK_SLOTS must be an integer from 1 to %d, got %r" % (MAX_SLOTS, raw)
        )
    return count


def poll_interval():
    raw = os.environ.get("PLX_CHECK_POLL")
    try:
        value = float(raw) if raw else POLL_INTERVAL
    except ValueError:
        value = POLL_INTERVAL
    return value if value > 0 else POLL_INTERVAL


def parse_args(argv):
    lock_path = None
    timeout = None
    exclusive = False
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
        elif arg == "--exclusive":
            exclusive = True
        else:
            sys.exit("check-lock: unrecognized argument %r (commands go after --)" % (arg,))
        i += 1
    cmd = argv[i:]
    if not cmd:
        sys.exit("usage: check-lock.py [--lock PATH] [--timeout SECONDS] [--exclusive] -- CMD...")
    if lock_path is None:
        lock_path = default_lock_path()
    return lock_path, timeout, exclusive, cmd


def slot_path(lock_path, index):
    return lock_path if index == 0 else "%s.%d" % (lock_path, index)


def existing_slot_count(lock_path):
    """One past the highest slot file that exists, so an exclusive run also covers slots
    that a run started with a larger PLX_CHECK_SLOTS created."""
    highest = 0
    for path in glob.glob(glob.escape(lock_path) + ".*"):
        suffix = path[len(lock_path) + 1:]
        if suffix.isdigit():
            highest = max(highest, int(suffix))
    return highest + 1


def try_flock(fd, op):
    try:
        fcntl.flock(fd, op | fcntl.LOCK_NB)
        return True
    except BlockingIOError:
        return False


def open_lock(path):
    return os.open(path, os.O_RDWR | os.O_CREAT, 0o644)


def write_identity(fd, exclusive, claimed_at):
    info = {
        "pid": os.getpid(),
        "worktree": os.getcwd(),
        "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "mode": "exclusive" if exclusive else "shared",
        # Epoch seconds at which the lock was won, not when this line was written: lets a test
        # (and a person reading the file) order two runs without sub-second guesswork.
        "epoch": claimed_at,
    }
    os.ftruncate(fd, 0)
    os.lseek(fd, 0, os.SEEK_SET)
    os.write(fd, json.dumps(info).encode("utf-8"))
    os.fsync(fd)


def read_identity(path):
    """Best-effort read of the identity a lock owner wrote. May be missing if the owner has
    not written it yet -- never authoritative, only a courtesy to whoever is waiting."""
    try:
        with open(path, "r") as fh:
            raw = fh.read().strip()
        return json.loads(raw) if raw else None
    except (OSError, ValueError):
        return None


def current_holders(paths):
    """The identities of whoever holds each lock file in `paths` right now. A file is held
    when a shared flock on it is refused (an exclusive flock is outstanding); the probe is
    dropped at once. Returns (identified holders deduplicated by pid, count of holders whose
    identity could not be read)."""
    seen = {}
    unknown = 0
    for path in paths:
        try:
            fd = open_lock(path)
        except OSError:
            continue
        try:
            if try_flock(fd, fcntl.LOCK_SH):
                fcntl.flock(fd, fcntl.LOCK_UN)
                continue
        finally:
            os.close(fd)
        info = read_identity(path)
        if info and "pid" in info:
            if info["pid"] != os.getpid():  # an exclusive request holds some of these itself
                seen[info["pid"]] = info
        else:
            unknown += 1
    return list(seen.values()), unknown


def format_waiting(holders, unknown):
    parts = []
    for info in holders:
        text = "pid %s in %s (started %s)" % (
            info.get("pid", "?"), info.get("worktree", "?"), info.get("started", "?"))
        if info.get("mode") == "exclusive":
            text += " [exclusive]"
        parts.append(text)
    if unknown:
        parts.append("%d holder(s) whose identity is not available yet" % unknown)
    if not parts:
        return "check-lock: waiting on another `make check` (holder identity not available yet)"
    return "check-lock: waiting on " + "; ".join(parts)


class Lease:
    """The fds a run holds. Closing them releases the flocks; the identity is emptied first
    so a waiter probing a just-released file does not name a run that has finished."""

    def __init__(self, slot_fds, gate_fd, gate_exclusive):
        self.slot_fds = slot_fds
        self.gate_fd = gate_fd
        self.gate_exclusive = gate_exclusive

    def close(self):
        for fd in self.slot_fds:
            os.ftruncate(fd, 0)
            os.close(fd)
        if self.gate_exclusive:
            os.ftruncate(self.gate_fd, 0)
        os.close(self.gate_fd)


def acquire(lock_path, timeout, exclusive, slots):
    lock_dir = os.path.dirname(lock_path)
    if lock_dir:
        os.makedirs(lock_dir, exist_ok=True)
    if exclusive:
        slots = max(slots, existing_slot_count(lock_path))
    paths = [slot_path(lock_path, i) for i in range(slots)]
    gate_path = lock_path + ".gate"
    fds = [open_lock(p) for p in paths]
    gate_fd = open_lock(gate_path)
    interval = poll_interval()
    deadline = None if timeout is None else time.monotonic() + timeout
    printed_first = False
    last_report = 0.0
    wait_start = time.monotonic()
    held = []  # exclusive: indices of the slots collected so far
    gate_held = False

    def give_up():
        for i in held:
            os.ftruncate(fds[i], 0)
        if gate_held:
            os.ftruncate(gate_fd, 0)
        for fd in fds:
            os.close(fd)
        os.close(gate_fd)

    while True:
        if exclusive:
            if not gate_held and try_flock(gate_fd, fcntl.LOCK_EX):
                gate_held = True
                write_identity(gate_fd, True, time.time())
            if gate_held:
                for i, fd in enumerate(fds):
                    if i not in held and try_flock(fd, fcntl.LOCK_EX):
                        held.append(i)
                        write_identity(fd, True, time.time())
                if len(held) == len(fds):
                    break
        elif try_flock(gate_fd, fcntl.LOCK_SH):
            # No exclusive request is pending. Hold the gate only while claiming a slot.
            claimed = None
            claimed_at = 0.0
            try:
                for i, fd in enumerate(fds):
                    if try_flock(fd, fcntl.LOCK_EX):
                        claimed = i
                        claimed_at = time.time()  # inside the gate: before any exclusive request can follow
                        break
            finally:
                fcntl.flock(gate_fd, fcntl.LOCK_UN)
            if claimed is not None:
                write_identity(fds[claimed], False, claimed_at)
                held = [claimed]
                break
        now = time.monotonic()
        timed_out = deadline is not None and now >= deadline
        if not printed_first or now - last_report >= REPORT_INTERVAL or timed_out:
            holders, unknown = current_holders([gate_path] + paths)
            text = format_waiting(holders, unknown)
            if timed_out:
                text = "check-lock: timed out after %gs waiting for the lock (%s)" % (timeout, text)
            print(text, file=sys.stderr)
            printed_first = True
            last_report = now
        if timed_out:
            give_up()
            sys.exit(EX_TEMPFAIL)
        time.sleep(interval)
    elapsed = time.monotonic() - wait_start
    if exclusive:
        print(
            "check-lock: acquired after %.0fs (exclusive: all %d slots)" % (elapsed, len(fds)),
            file=sys.stderr,
        )
        return Lease(fds, gate_fd, True)
    print(
        "check-lock: acquired after %.0fs (slot %d of %d)" % (elapsed, held[0] + 1, len(fds)),
        file=sys.stderr,
    )
    for i, fd in enumerate(fds):
        if i != held[0]:
            os.close(fd)
    return Lease([fds[held[0]]], gate_fd, False)


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
    lock_path, timeout, exclusive, cmd = parse_args(argv)
    if os.environ.get("PLX_CHECK_LOCK") == "off":
        return run_locked(cmd)
    lease = acquire(lock_path, timeout, exclusive, slot_count())
    try:
        return run_locked(cmd)
    finally:
        # Closing the fds releases the flocks; no explicit LOCK_UN needed, and this way a
        # crash between acquire() and here still releases them via process exit.
        lease.close()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
