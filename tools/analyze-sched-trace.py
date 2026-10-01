#!/usr/bin/env python3
"""Line slow FRAMEDROP frames up with a kernel event trace (the text `trace` format).

  analyze-sched-trace.py APP.log TRACE[.gz] --tid FRAME_TID [--sm SURFACE_MANAGER_COMM]
                         [--vsync-irq N] [--slow 20] [--offset-ms 0]
  analyze-sched-trace.py --self-test

Inputs
  APP.log   the app's event log with `--arm framecb` FRAMEDROP lines:
            `... seq=N mono=<ms> wait_at=<ms> w=<wall>/<cpu>/<runq> wsw=<slept>/<preempted>
             d=... dsw=... s=... ssw=... cb=...`
            mono= is CLOCK_MONOTONIC ms after the swap; wait_at= the same clock when the frame's
            first framebuffer command (the back-buffer wait) began.
  TRACE     ftrace text with trace_clock=mono, as tools/tv-sched-trace.sh captures it: sched_switch,
            sched_wakeup, irq_handler_entry (and optionally raw_syscalls sys_enter/sys_exit for the
            frame thread, which `tv-sched-trace.sh --tid N` enables -- without them the syscall a
            thread left the CPU in is simply not named).
  --tid     the frame thread (the app's main thread: tid == pid).

For every frame of --slow ms or more it prints, for the wait [wait_at, wait_at+w] and for the
commit phase after it:
  * each stretch the frame thread was OFF the CPU: how it left (preempted = still runnable, or
    blocked), the syscall it was inside, who ran on that CPU meanwhile, and who woke it;
  * the vsync interrupts and Mali interrupts that fired in the window, and when surface-manager's
    threads ran, so "GPU idle, compositor not yet run" is told from "GPU busy".

Host-only: reads files, touches no device.
"""
import argparse
import bisect
import collections
import contextlib
import gzip
import io
import os
import re
import sys
import tempfile

# ARM EABI numbers: the app is a 32-bit task, and raw_syscalls reports the compat number.
SYSCALL = {3: "read", 4: "write", 54: "ioctl", 118: "fsync", 142: "select", 146: "writev",
           162: "nanosleep", 168: "poll", 240: "futex", 252: "epoll_wait", 265: "clock_nanosleep",
           296: "sendmsg", 297: "recvmsg", 336: "ppoll", 346: "epoll_pwait"}
LINE = re.compile(r"^\s*(.+?)-(\d+)\s+\[(\d+)\]\s+\S+\s+([\d.]+):\s+(\w+):\s*(.*)$")
SW = re.compile(r"prev_comm=(.+?) prev_pid=(\d+) prev_prio=\d+ prev_state=(\S+) "
                r"==> next_comm=(.+?) next_pid=(\d+)")
FRAME = re.compile(r"total=([\d.]+).* seq=(\d+) mono=([\d.]+)"
                   r"(?: wait_at=([\d.]+) w=([\d.]+)/([\d.]+)/([\d.]+))?")
GPU_IRQ = ("mali", "kbase", "gpu")


def load_trace(path):
    """(t_ms, cpu, comm, pid, event, rest) per parsed line."""
    ev = []
    opener = gzip.open if path.endswith(".gz") else open
    with opener(path, "rt", errors="replace") as f:
        for line in f:
            m = LINE.match(line)
            if m:
                ev.append((float(m[4]) * 1000.0, int(m[3]), m[1].strip(), int(m[2]), m[5], m[6]))
    return ev


def load_frames(path, slow):
    out = []
    with open(path, errors="replace") as f:
        for line in f:
            if not line.startswith("FRAMEDROP"):
                continue
            m = FRAME.search(line)
            if not m or float(m[1]) < slow or not m[4]:
                continue
            out.append(dict(total=float(m[1]), seq=int(m[2]), end=float(m[3]),
                            wait_at=float(m[4]), w=float(m[5]), wcpu=float(m[6])))
    return out


def window(ev, times, a, b):
    return ev[bisect.bisect_left(times, a):bisect.bisect_right(times, b)]


def describe(ev, times, tid, sm, vsync_irq, a, b, label):
    print(f"  -- {label}: {a:.2f} .. {b:.2f} ms ({b - a:.2f} ms)")
    off = None  # (t, cpu, how, syscall, next comm, next pid)
    in_sys = None
    for t, cpu, _comm, pid, name, rest in window(ev, times, a - 2.0, b + 0.5):
        if name == "sys_enter" and pid == tid:
            nr = int(re.search(r"NR (\d+)", rest)[1])
            in_sys = SYSCALL.get(nr, f"nr{nr}")
        elif name == "sys_exit" and pid == tid:
            in_sys = None
        elif name == "sched_switch":
            m = SW.search(rest)
            if not m:
                continue
            if int(m[2]) == tid:
                how = "PREEMPTED (runnable)" if m[3].startswith("R") else f"BLOCKED state={m[3]}"
                off = (t, cpu, how, in_sys, m[4], int(m[5]))
            elif int(m[5]) == tid and off:
                t0, c0, how, sysc, nxt, npid = off
                if t >= a and t0 <= b:
                    ran = collections.Counter()
                    last = (t0, f"{nxt}-{npid}")
                    for t2, c2, _, _, n2, r2 in window(ev, times, t0, t):
                        if n2 == "sched_switch" and c2 == c0:
                            m2 = SW.search(r2)
                            if m2:
                                ran[last[1]] += t2 - last[0]
                                last = (t2, f"{m2[4]}-{m2[5]}")
                    top = ", ".join(f"{k} {v:.2f}ms" for k, v in ran.most_common(4))
                    waker = next((f"{c}-{p}" for t2, _, c, p, n2, r2 in reversed(window(ev, times, t0, t))
                                  if n2 == "sched_wakeup" and re.search(rf"pid={tid}\b", r2)), "?")
                    print(f"     off-CPU {t0:.2f}..{t:.2f} ({t - t0:.2f} ms) cpu{c0}->cpu{cpu}: {how}"
                          f"{' in ' + sysc if sysc else ''}; woken by {waker}; cpu{c0} ran: {top or 'idle'}")
                off = None
    irqs = [(t, re.search(r"irq=(\d+) name=(\S+)", r))
            for t, _, _, _, n, r in window(ev, times, a, b) if n == "irq_handler_entry"]
    vs = [t for t, m in irqs if m and (int(m[1]) == vsync_irq or "osd" in m[2])]
    gpu = [t for t, m in irqs if m and any(k in m[2] for k in GPU_IRQ)]
    span = (f" first/last {gpu[0] - a:.2f}/{gpu[-1] - a:.2f}" if gpu
            else " -> GPU raised no interrupt in this window")
    print(f"     vsync irqs at {[round(t - a, 2) for t in vs]} (ms into window); GPU irqs: {len(gpu)}{span}")
    if sm:
        runs = [(t, SW.search(r)) for t, _, _, _, n, r in window(ev, times, a, b) if n == "sched_switch"]
        on = [round(t - a, 2) for t, m in runs if m and m[4].startswith(sm)]
        print(f"     {sm}* threads scheduled in at {on[:12]}{' ...' if len(on) > 12 else ''}")


def analyze(log, trace, tid, sm, vsync_irq, slow, offset_ms):
    ev = load_trace(trace)
    if not ev:
        sys.exit("no trace events parsed")
    ev = [(t - offset_ms, *rest) for t, *rest in ev]
    times = [e[0] for e in ev]
    frames = load_frames(log, slow)
    covered = [f for f in frames if times[0] <= f["wait_at"] and f["end"] <= times[-1]]
    print(f"trace {times[0]:.1f}..{times[-1]:.1f} ms mono, {len(ev)} events; "
          f"slow frames in log {len(frames)}, inside the trace {len(covered)}")
    for f in covered:
        print(f"\nseq={f['seq']} total={f['total']} wait={f['w']} ms (cpu {f['wcpu']})")
        describe(ev, times, tid, sm, vsync_irq, f["wait_at"], f["wait_at"] + f["w"], "back-buffer wait")
        describe(ev, times, tid, sm, vsync_irq, f["wait_at"] + f["w"], f["end"],
                 "commit phase (draw + swap)")


# One slow frame (seq 5, wait 1002..1012 ms): the frame thread (tid 100) blocks in futex at
# 1004 ms, kworker runs on its CPU, surface-manager wakes it at 1009 ms and it runs again at 1010.
SELF_TEST_LOG = ("FRAMEDROP total=30.0 ingest=0.1 up=0 seq=5 mono=1015.0 wait_at=1002.00 "
                 "w=10.00/1.00/0.00 wsw=1/0 d=1.0/1.0/0.0 dsw=0/0 s=1.0/1.0/0.0 ssw=0/0 cb=\n")
SELF_TEST_TRACE = """\
 app-100 [000] d..2 1.001000: sched_wakeup: comm=app pid=100 prio=120 target_cpu=000
 app-100 [000] .... 1.003500: sys_enter: NR 240 (0, 80, 0, 0, 0, 0)
 app-100 [000] d..2 1.004000: sched_switch: prev_comm=app prev_pid=100 prev_prio=120 prev_state=S ==> next_comm=kworker next_pid=7 next_prio=120
 swapper-0 [001] d..2 1.005000: sched_switch: prev_comm=swapper prev_pid=0 prev_prio=120 prev_state=R ==> next_comm=surface-manager next_pid=50 next_prio=120
 kworker-7 [000] d.h1 1.006000: irq_handler_entry: irq=45 name=osd_irq
 kworker-7 [000] d.h1 1.007000: irq_handler_entry: irq=60 name=mali
 surface-manager-50 [001] d..2 1.009000: sched_wakeup: comm=app pid=100 prio=120 target_cpu=000
 kworker-7 [000] d..2 1.010000: sched_switch: prev_comm=kworker prev_pid=7 prev_prio=120 prev_state=R ==> next_comm=app next_pid=100 next_prio=120
 app-100 [000] d..2 1.016000: sched_switch: prev_comm=app prev_pid=100 prev_prio=120 prev_state=R ==> next_comm=swapper next_pid=0 next_prio=120
"""


def self_test():
    with tempfile.TemporaryDirectory() as d:
        log, trace, gz = (os.path.join(d, n) for n in ("app.log", "trace", "trace.gz"))
        with open(log, "w") as f:
            f.write("unrelated line\n" + SELF_TEST_LOG)
        with open(trace, "w") as f:
            f.write(SELF_TEST_TRACE)
        with gzip.open(gz, "wt") as f:
            f.write(SELF_TEST_TRACE)
        outs = []
        for t in (trace, gz):
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                analyze(log, t, 100, "surface-manager", -1, 20.0, 0.0)
            outs.append(buf.getvalue())
    assert outs[0] == outs[1], "plain and gzip traces must read the same"
    out = outs[0]
    for want in ("slow frames in log 1, inside the trace 1", "seq=5 total=30.0 wait=10.0",
                 "BLOCKED state=S in futex", "woken by surface-manager-50", "kworker-7 6.00ms",
                 "vsync irqs at [4.0]", "GPU irqs: 1", "surface-manager* threads scheduled in at [3.0]"):
        assert want in out, f"missing {want!r} in:\n{out}"
    # Below --slow nothing is reported, and an unarmed log (no wait_at=) is skipped, not crashed on.
    with tempfile.TemporaryDirectory() as d:
        log = os.path.join(d, "app.log")
        with open(log, "w") as f:
            f.write("FRAMEDROP total=30.0 seq=1 mono=5.0\nFRAMEDROP total=10.0 seq=2 mono=9.0 wait_at=1.0 w=1/1/0\n")
        assert load_frames(log, 20.0) == []
    print("analyze-sched-trace self-test: ok")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("log", nargs="?")
    ap.add_argument("trace", nargs="?")
    ap.add_argument("--tid", type=int)
    ap.add_argument("--sm", default="surface-manager", help="comm prefix of the compositor's threads")
    ap.add_argument("--vsync-irq", type=int, default=-1, help="IRQ number of the vsync line (`osd_irq` is matched by name too)")
    ap.add_argument("--slow", type=float, default=20.0, help="report frames of at least this many ms")
    ap.add_argument("--offset-ms", type=float, default=0.0, help="trace clock minus CLOCK_MONOTONIC, if not mono")
    ap.add_argument("--self-test", action="store_true", help="run against a synthetic trace and exit")
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    if not (a.log and a.trace and a.tid):
        ap.error("APP.log, TRACE and --tid are required")
    analyze(a.log, a.trace, a.tid, a.sm, a.vsync_irq, a.slow, a.offset_ms)


if __name__ == "__main__":
    main()
