#!/usr/bin/env python3
"""Line slow FRAMEDROP frames up with a kernel event trace (the text `trace` format).

  analyze-sched-trace.py APP.log TRACE[.gz] --tid FRAME_TID [--sm SURFACE_MANAGER_COMM]
                         [--vsync-irq N] [--slow 20] [--offset-ms 0]
  analyze-sched-trace.py APP.log TRACE[.gz]... --tid FRAME_TID --latch [--wait-min 10]
                         [--app-gpu-tid N --sm-gpu-tid N]
  analyze-sched-trace.py --self-test

Inputs
  APP.log   the app's event log with `--arm framecb` FRAMEDROP lines:
            `... seq=N mono=<ms> wait_at=<ms> w=<wall>/<cpu>/<runq> wsw=<slept>/<preempted>
             d=... dsw=... s=... ssw=... p=... psw=... cb=...`
            (`p=` is the frame loop's `prepare_window`, same units as `w=`; it precedes the wait,
            may appear without `wait_at=`, and is not read here)
            mono= is CLOCK_MONOTONIC ms after the swap; wait_at= the same clock when the frame's
            first framebuffer command (the back-buffer wait) began.
  TRACE     ftrace text with trace_clock=mono, as tools/tv-sched-trace.sh captures it: sched_switch,
            sched_wakeup, irq_handler_entry (and optionally raw_syscalls sys_enter/sys_exit for the
            frame thread, which `tv-sched-trace.sh --tid N` enables -- without them the syscall a
            thread left the CPU in is simply not named).
  --tid     the frame thread (the app's main thread: tid == pid).
  The thread table (`# plx-task tgid=N tid=N comm=NAME` lines at the head of a trace that
  tools/tv-sched-trace.sh wrote) maps a thread to its process; --latch needs it, or the two
  --*-gpu-tid options, to say whose GPU job completed. Older traces carry none and analyse as before.

For every frame of --slow ms or more it prints, for the wait [wait_at, wait_at+w] and for the
commit phase after it:
  * each stretch the frame thread was OFF the CPU: how it left (preempted = still runnable, or
    blocked), the syscall it was inside, who ran on that CPU meanwhile, and who woke it. A wakeup
    from another CPU shows up as woken by `<idle>-0` (the IPI path): that is not the waker, and the
    trace does not record who was;
  * the vsync interrupts and Mali interrupts that fired in the window, and when surface-manager's
    threads ran, so "GPU idle, compositor not yet run" is told from "GPU busy".

--latch answers a different question: for a back-buffer wait of --wait-min ms or more, was it the
app's CPU, the app's GPU work, or the compositor that made the frame miss its refresh? It scores the
frame pairs (k, k+1) whose two FRAMEDROP lines both carry `wait_at=` (`plxnative-framecb`). The
recipe is `plxnative-framecb` with `plxnative-framering=17`: the ring writes every frame of 17 ms or
more with the 4 frames before it and the 3 after (`diag::heartbeat::FrameRing`), so the pairs around
every slow frame are in the log, and it only works while `plxnative-framedrop` is armed (the fps
harness arms it for a scene with a frame ceiling; add `plxnative-framedrop=17` otherwise).
`plxnative-framedrop=0.01` would log every frame instead, but at about 60 log-file open/append/close
round trips a second ON THE FRAME THREAD, in the phase being measured. One or more TRACEs of the
same run may be given; the summary spans them.
  * The compositor (surface-manager) composes at 60 Hz, just after every other `osd_irq` (they fire
    every 8.33 ms), and takes the app's newest buffer if the app's GPU work on it is done; taking
    buffer k releases k-1 at once. Frame k's wait ends just after one such tick; the tick that
    should take frame k's buffer is that tick + 16.67 ms: the "aim" (the osd_irq nearest it: the
    compositor's phase drifts, and it can take a buffer at either osd tick).
  * Mali defers the fragment work to the swap, so frame k's GPU work runs AFTER its commit
    (FRAMEDROP `mono=`). "GPU done" is the last app job completion after max(commit_k, frame k+1's
    wait start) and before frame k+1's wait ends. A completion is a `mali` irq followed within
    0.6 ms by a wakeup of a `mali-event-hnd` thread, which belongs to the process that owned the job.
  * "ticks" is how far frame k+1's release slipped past aim + 0.5 ms, in 8.33 ms osd ticks (2 = a
    whole missed refresh). Class: CPU-late (commit_k after aim), GPU-late (commit on time, GPU done
    more than 0.65 ms after aim), edge (GPU done in (aim, aim + 0.65]: the compositor takes the
    buffer or not by a hair), compositor (both before aim, the release still came late).
  * The headline is the margin: GPU done minus aim over the on-time (0-tick) waited frames the log
    holds (with the ring, those near a slow frame, not every frame of the run). About zero or
    positive means the frame is one hiccup from a missed refresh.

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
RUNQ = re.compile(r" d=[\d.]+/[\d.]+/([\d.]+) .*? s=[\d.]+/[\d.]+/([\d.]+) ")  # draw / swap run-queue ms
IRQ = re.compile(r"irq=(\d+) name=(\S+)")
WAKE = re.compile(r"comm=(.+?) pid=(\d+)")
# The thread table tools/tv-sched-trace.sh writes ahead of the events: one line per thread of the
# traced process and of every surface-manager process (a ftrace line names a pid, never its tgid).
TASK = re.compile(r"^# plx-task tgid=(\d+) tid=(\d+) comm=(.*?)\s*$")
GPU_IRQ = ("mali", "kbase", "gpu")


def load_trace(path, tasks=None):
    """(t_ms, cpu, comm, pid, event, rest) per parsed line. Thread-table lines go into `tasks`
    ({tid: (tgid, comm)}) when a dict is passed."""
    ev = []
    opener = gzip.open if path.endswith(".gz") else open
    with opener(path, "rt", errors="replace") as f:
        for line in f:
            m = LINE.match(line)
            if m:
                ev.append((float(m[4]) * 1000.0, int(m[3]), m[1].strip(), int(m[2]), m[5], m[6]))
            elif tasks is not None and line.startswith("# plx-task"):
                m = TASK.match(line)
                if m:
                    tasks[int(m[2])] = (int(m[1]), m[3])
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
            rq = RUNQ.search(line)
            out.append(dict(total=float(m[1]), seq=int(m[2]), end=float(m[3]),
                            wait_at=float(m[4]), w=float(m[5]), wcpu=float(m[6]),
                            draw_runq=float(rq[1]) if rq else 0.0, swap_runq=float(rq[2]) if rq else 0.0))
    return out


def irq_kind(rest, vsync_irq):
    """"vsync", "gpu" or None for the text of an irq_handler_entry event."""
    m = IRQ.search(rest)
    if not m:
        return None
    if int(m[1]) == vsync_irq or "osd" in m[2]:
        return "vsync"
    return "gpu" if any(k in m[2] for k in GPU_IRQ) else None


def waker_label(comm, pid):
    """`comm-pid` of a wakeup's recorded waker. A wakeup from another CPU is recorded in hardirq
    context as woken by that CPU's idle task (`<idle>-0`, the IPI path): that is the plumbing, not who
    woke the thread, and the trace does not say who did."""
    label = f"{comm}-{pid}"
    if comm == "<idle>" and pid == 0:
        label += " (cross-CPU wakeup via IPI: not the waker, which the trace does not record)"
    return label


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
                    waker = next((waker_label(c, p) for t2, _, c, p, n2, r2 in reversed(window(ev, times, t0, t))
                                  if n2 == "sched_wakeup" and re.search(rf"pid={tid}\b", r2)), "?")
                    print(f"     off-CPU {t0:.2f}..{t:.2f} ({t - t0:.2f} ms) cpu{c0}->cpu{cpu}: {how}"
                          f"{' in ' + sysc if sysc else ''}; woken by {waker}; cpu{c0} ran: {top or 'idle'}")
                off = None
    irqs = [(t, irq_kind(r, vsync_irq))
            for t, _, _, _, n, r in window(ev, times, a, b) if n == "irq_handler_entry"]
    vs = [t for t, k in irqs if k == "vsync"]
    gpu = [t for t, k in irqs if k == "gpu"]
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


# ---- --latch ----------------------------------------------------------------------------------------
# Constants, ms. The measured ones say where and when they were measured.
TICK_MS = 8.333          # osd_irq period: two per 60 Hz refresh (926 of 928 gaps, 2026-10-02)
REFRESH_MS = 16.67       # the compositor composes at every other osd_irq: one refresh
RELEASE_LAG_MS = 0.5     # the frame thread wakes ~0.5 ms after the osd_irq whose composition freed its buffer
RELEASE_WINDOW_MS = 2.0  # a wait that ends within this of an osd_irq ended because of it
JOB_WAKE_MS = 0.6        # a mali-event-hnd wakeup within this of a mali irq is that irq's job completing
EDGE_MS = 0.65           # GPU done in (aim, aim + EDGE_MS] is where frames that DID make the refresh also
#                          land: the p95 of GPU-done minus aim over main's on-time grid-pong frames was
#                          +0.65 / +0.67 (two legs, 2026-10-24). A miss there is a coin flip, not a late GPU.
MIN_WAIT_MS = 1.0        # a shorter wait is not a wait: the buffer was free already, so there is no release tick
TRACE_START_GUARD_MS = 2.0   # a frame this close to the first or last event of a window is cut off:
TRACE_END_GUARD_MS = 1.0     # its release or its jobs may lie outside the trace
WAIT_MIN_MS = 10.0       # default --wait-min. Half a refresh is a 12-15 ms wait, a whole one 20-22 ms,
#                          against grid-pong's 6-7 ms median wait.


def quantile(sorted_values, q):
    n = len(sorted_values)
    return sorted_values[min(n - 1, int(n * q))] if n else float("nan")


def nearest(times, t):
    """The element of the sorted `times` closest to t."""
    i = bisect.bisect_left(times, t)
    return min((times[j] for j in (i - 1, i) if 0 <= j < len(times)), key=lambda v: abs(v - t))


def gpu_owner(tasks, tid, sm, app_gpu_tid, sm_gpu_tid):
    """(owner, how) where owner(mali-event-hnd tid) is "app", "compositor" or "other"; or (None, why).
    Explicit tids win; else the trace's thread table decides by process (tgid)."""
    if app_gpu_tid is not None or sm_gpu_tid is not None:
        return (lambda t: "app" if t == app_gpu_tid else "compositor" if t == sm_gpu_tid else "other",
                "--app-gpu-tid/--sm-gpu-tid")
    if tasks:
        app_tgid = tasks[tid][0] if tid in tasks else tid
        sm_tgids = {g for t, (g, comm) in tasks.items() if t == g and comm.startswith(sm)}
        return (lambda t: "app" if tasks.get(t, (None,))[0] == app_tgid
                else "compositor" if tasks.get(t, (None,))[0] in sm_tgids else "other",
                f"thread table, {len(tasks)} threads")
    return (None, "the trace has no thread table (tv-sched-trace.sh writes one) and neither "
                  "--app-gpu-tid nor --sm-gpu-tid was given")


def scan_events(ev, vsync_irq):
    """(osd_irq times, mali irq times, [(t, tid)] of mali-event-hnd wakeups), each sorted by time."""
    osd, mali, wakes = [], [], []
    for t, _cpu, _comm, _pid, name, rest in ev:
        if name == "irq_handler_entry":
            kind = irq_kind(rest, vsync_irq)
            if kind == "vsync":
                osd.append(t)
            elif kind == "gpu":
                mali.append(t)
        elif name == "sched_wakeup":
            m = WAKE.search(rest)
            if m and m[1] == "mali-event-hnd":
                wakes.append((t, int(m[2])))
    return sorted(osd), sorted(mali), sorted(wakes)


def job_completions(mali, wakes):
    """[(mali irq time, mali-event-hnd tid)]: the kbase completion work woke that thread, so the irq
    finished a job of the thread's process. Wakeups with no Mali irq just before them are not jobs."""
    jobs = []
    for t, tid in wakes:
        i = bisect.bisect_right(mali, t) - 1
        if i >= 0 and t - mali[i] <= JOB_WAKE_MS:
            jobs.append((mali[i], tid))
    return jobs


def score_pairs(frames, osd, app_jobs, t0, t1):
    """Score each pair of consecutive frames (k, k+1) in {seq: frame}; (pairs, rows).

    Frame k's wait ended just after an osd tick; `aim` is the tick nearest that + one refresh, the one
    that should take frame k. A row is one pair whose frame k+1 waited at least MIN_WAIT_MS: how many
    osd ticks k+1's release slipped past aim, where k committed and where its GPU work finished
    (the app's last job completion after the later of k's commit and k+1's wait start, before k+1's
    release; None if there is none or `app_jobs` is None). `pairs` counts every pair with an aim."""
    pairs, rows = 0, []
    for k in sorted(frames):
        f, g = frames[k], frames.get(k + 1)
        if (not g or f["wait_at"] < t0 + TRACE_START_GUARD_MS or f["w"] < MIN_WAIT_MS
                or g["wait_at"] + g["w"] > t1 - TRACE_END_GUARD_MS):
            continue
        woke = f["wait_at"] + f["w"]
        i = bisect.bisect_right(osd, woke) - 1
        if i < 0 or woke - osd[i] > RELEASE_WINDOW_MS:
            continue   # frame k's wake does not follow an osd tick: no latch to aim at
        pairs += 1
        if g["w"] < MIN_WAIT_MS:
            continue
        aim = nearest(osd, osd[i] + REFRESH_MS)
        rel = g["wait_at"] + g["w"]
        gpu = None
        if app_jobs:
            lo = bisect.bisect_right(app_jobs, max(f["end"], g["wait_at"]))
            hi = bisect.bisect_left(app_jobs, rel)
            gpu = app_jobs[hi - 1] if hi > lo else None
        rows.append(dict(seq=k, wait=g["w"], ticks=round((rel - (aim + RELEASE_LAG_MS)) / TICK_MS),
                         commit_aim=f["end"] - aim, wake_commit=f["end"] - woke,
                         gpu_aim=None if gpu is None else gpu - aim,
                         gpu_commit=None if gpu is None else gpu - f["end"],
                         draw_runq=f["draw_runq"], swap_runq=f["swap_runq"]))
    return pairs, rows


def latch_class(row, have_gpu):
    if row["commit_aim"] > 0:
        return "CPU-late"
    if not have_gpu:
        return "unattributed"
    if row["gpu_aim"] is not None and row["gpu_aim"] > EDGE_MS:
        return "GPU-late"
    if row["gpu_aim"] is not None and row["gpu_aim"] > 0:
        return "edge"
    return "compositor"


def ticks_label(n):
    return f"{n:+d} tick{'' if abs(n) == 1 else 's'}"


def latch_trace(path, frames, args, acc):
    tasks = {}
    ev = load_trace(path, tasks)
    if not ev:
        sys.exit(f"{path}: no trace events parsed")
    ev = [(t - args.offset_ms, *rest) for t, *rest in ev]
    t0, t1 = ev[0][0], ev[-1][0]
    osd, mali, wakes = scan_events(ev, args.vsync_irq)
    print(f"{path}: {t0:.1f}..{t1:.1f} ms mono ({(t1 - t0) / 1000:.1f} s), {len(ev)} events, "
          f"{len(osd)} osd_irq")
    owner, how = gpu_owner(tasks, args.tid, args.sm, args.app_gpu_tid, args.sm_gpu_tid)
    app_jobs = None
    if owner is None:
        print(f"  mali irqs {len(mali)}; GPU columns skipped: {how}")
    else:
        jobs = [(t, owner(tid)) for t, tid in job_completions(mali, wakes)]
        by = collections.Counter(o for _, o in jobs)
        print(f"  mali irqs {len(mali)}; job completions attributed ({how}): app {by['app']}, "
              f"compositor {by['compositor']}, other {by['other']}")
        if not by["app"]:
            print("  WARNING: no job completion belongs to the app (wrong --tid or gpu tid?); "
                  "every GPU number below is empty")
        app_jobs = sorted(t for t, o in jobs if o == "app")
    pairs, rows = score_pairs(frames, osd, app_jobs, t0, t1)
    acc["pairs"] += pairs
    for r in rows:
        r["cls"] = latch_class(r, owner is not None)
        acc["rows"].append(r)
        if r["ticks"] == 0 and r["gpu_aim"] is not None:
            acc["margin"].append(r)
        if r["wait"] >= args.wait_min:
            gpu = "  none" if r["gpu_aim"] is None else f"{r['gpu_aim']:+6.2f}"
            print(f"  seq {r['seq']}->{r['seq'] + 1}: wait {r['wait']:5.1f}  {ticks_label(r['ticks']):8} "
                  f"{r['cls']:12} commit-aim {r['commit_aim']:+6.2f}  gpu-aim {gpu}  "
                  f"wake->commit {r['wake_commit']:5.2f} "
                  f"(draw runq {r['draw_runq']:.2f}, swap runq {r['swap_runq']:.2f})")


def latch(log, traces, args):
    frames = {f["seq"]: f for f in load_frames(log, 0.0)}
    if not frames:
        sys.exit(f"{log}: no FRAMEDROP line carries wait_at= (arm plxnative-framecb, with plxnative-framering=17 "
                 "so the slow frames and their neighbours are logged)")
    acc = dict(pairs=0, rows=[], margin=[])
    for path in traces:
        latch_trace(path, frames, args, acc)
    long_waits = [r for r in acc["rows"] if r["wait"] >= args.wait_min]
    print(f"\nover {len(traces)} trace(s): {acc['pairs']} frame pairs scored; waits >= {args.wait_min:g} ms: "
          f"{len(long_waits)}")
    for cls in sorted({r["cls"] for r in long_waits}):
        ticks = collections.Counter(r["ticks"] for r in long_waits if r["cls"] == cls)
        print(f"  {cls:12} {sum(ticks.values()):3}  ("
              + ", ".join(f"{ticks_label(n)} x{c}" for n, c in sorted(ticks.items())) + ")")
    m = acc["margin"]
    if not m:
        print("on-time (0-tick) waited frames: no GPU done times (see the notes above)")
        return
    aim = sorted(r["gpu_aim"] for r in m)
    commit = sorted(r["gpu_commit"] for r in m)
    print(f"on-time (0-tick) waited frames: n={len(m)}; GPU done minus aim p05/p50/p95/p99 "
          f"{quantile(aim, .05):+.2f}/{quantile(aim, .5):+.2f}/{quantile(aim, .95):+.2f}/"
          f"{quantile(aim, .99):+.2f} ms; GPU done minus commit p50 {quantile(commit, .5):+.2f} ms")


# One slow frame (seq 5, wait 1002..1012 ms): the frame thread (tid 100) blocks in futex at
# 1004 ms, kworker runs on its CPU, surface-manager wakes it at 1009 ms and it runs again at 1010.
SELF_TEST_LOG = ("FRAMEDROP total=30.0 ingest=0.1 up=0 seq=5 mono=1015.0 wait_at=1002.00 "
                 "w=10.00/1.00/0.00 wsw=1/0 d=1.0/1.0/0.0 dsw=0/0 s=1.0/1.0/0.0 ssw=0/0 "
                 "p=2.00/1.50/0.10 psw=0/1 cb=\n")
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
    assert waker_label("kworker", 7) == "kworker-7"
    assert waker_label("<idle>", 0).startswith("<idle>-0 (cross-CPU wakeup via IPI: not the waker"), \
        "an <idle> waker is the IPI path, not a waker"
    latch_self_test()
    print("analyze-sched-trace self-test: ok")


# --latch: eleven frames at 60 Hz (seq 1..11), frame i's buffer released just after osd tick r_i. Row i
# is what frame i does and what happens to the NEXT frame: (name, commit ms after its release, GPU done
# ms after its aim, ticks frame i+1's release slips past the aim, ms from the commit to frame i+1's
# wait start). Pair 3->4 is GPU-late by two ticks (a whole refresh), 5->6 compositor-side, 7->8 CPU-late
# (committed 0.83 ms after the aim), 9->10 an edge case a tick late; the six others are on time.
LATCH_FRAMES = [
    ("on-time", 5.0, -0.5, 0, 5.0), ("on-time", 5.0, +0.2, 0, 5.0), ("GPU-late", 5.0, +4.2, 2, 1.0),
    ("on-time", 5.0, +0.3, 0, 5.0), ("compositor", 5.0, -1.5, 2, 1.0), ("on-time", 5.0, -0.3, 0, 5.0),
    ("CPU-late", 17.0, +3.8, 2, 1.0), ("on-time", 5.0, +0.1, 0, 5.0), ("edge", 5.0, +0.4, 1, 1.0),
    ("on-time", 5.0, +0.4, 0, 5.0), ("last", 5.0, +0.2, 0, 5.0),
]
LATCH_APP, LATCH_APP_GPU, LATCH_SM, LATCH_SM_GPU, LATCH_OTHER_GPU = 100, 101, 50, 51, 77
LATCH_TABLE = (f"# plx-task tgid={LATCH_APP} tid={LATCH_APP} comm=app\n"
               f"# plx-task tgid={LATCH_APP} tid={LATCH_APP_GPU} comm=mali-event-hnd\n"
               f"# plx-task tgid={LATCH_SM} tid={LATCH_SM} comm=surface-manager\n"
               f"# plx-task tgid={LATCH_SM} tid={LATCH_SM_GPU} comm=mali-event-hnd\n")


def synthetic_latch():
    """(FRAMEDROP log text, trace body text) for LATCH_FRAMES. A job is a Mali irq and, 0.1 ms later, a
    wakeup of its owner's mali-event-hnd; frame 2's wakeup sits on the SAME microsecond as its irq and
    is written first, the way ftrace orders a tie."""
    osd = [1000.0 + j * TICK_MS for j in range(36)]
    log, ev = [], []

    def line(t, comm, pid, name, rest):
        ev.append((t, f" {comm}-{pid} [001] d..2 {t / 1000:.6f}: {name}: {rest}\n"))

    def job(t, tid, tie=False):
        wake = f"comm=mali-event-hnd pid={tid} prio=120 target_cpu=001"
        if tie:
            line(t, "<idle>", 0, "sched_wakeup", wake)
            line(t, "<idle>", 0, "irq_handler_entry", "irq=126 name=mali.0")
        else:
            line(t, "<idle>", 0, "irq_handler_entry", "irq=126 name=mali.0")
            line(t + 0.1, "<idle>", 0, "sched_wakeup", wake)

    r, at, end, gap = 3, None, None, None
    for i, (_name, cpu, gpu_off, ext, next_gap) in enumerate(LATCH_FRAMES):
        rel = osd[r] + RELEASE_LAG_MS
        at = rel - 6.0 if at is None else end + gap
        end = rel + cpu
        aim = osd[r + 2]
        log.append(f"FRAMEDROP total=16.7 ingest=0.1 seq={i + 1} mono={end:.3f} wait_at={at:.3f} "
                   f"w={rel - at:.3f}/0.20/0.00 wsw=0/0 d=3.00/2.50/0.31 dsw=0/0 s=0.50/0.40/0.07 "
                   f"ssw=0/0 p=1.00/0.80/0.00 psw=0/0 cb=\n")
        job(aim + gpu_off, LATCH_APP_GPU, tie=(i == 1))
        job(aim + 1.0, LATCH_SM_GPU)
        r, gap = r + 2 + ext, next_gap
    job(osd[10] + 3.0, LATCH_OTHER_GPU)
    for t in osd:
        line(t, "<idle>", 0, "irq_handler_entry", "irq=104 name=osd_irq")
    sw = "prev_comm=a prev_pid=1 prev_prio=120 prev_state=S ==> next_comm=b next_pid=2 next_prio=120"
    line(990.0, "swapper", 0, "sched_switch", sw)
    line(osd[-1], "swapper", 0, "sched_switch", sw)
    ev.sort(key=lambda e: e[0])   # stable: the tie keeps its order
    return "".join(log), "".join(text for _, text in ev)


def run_latch(log, trace, **over):
    args = argparse.Namespace(tid=LATCH_APP, sm="surface-manager", vsync_irq=-1, offset_ms=0.0,
                              wait_min=WAIT_MIN_MS, app_gpu_tid=None, sm_gpu_tid=None)
    vars(args).update(over)
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        latch(log, [trace], args)
    return buf.getvalue()


def latch_self_test():
    log_text, body = synthetic_latch()
    with tempfile.TemporaryDirectory() as d:
        log, plain, table, gz = (os.path.join(d, n) for n in ("app.log", "plain", "table", "table.gz"))
        with open(log, "w") as f:
            f.write("unrelated line\n" + log_text)
        with open(plain, "w") as f:
            f.write(body)
        with open(table, "w") as f:
            f.write(LATCH_TABLE + body)
        with gzip.open(gz, "wt") as f:
            f.write(LATCH_TABLE + body)
        tasks = {}
        assert len(load_trace(gz, tasks)) == len(load_trace(plain)) and len(tasks) == 4
        assert tasks[LATCH_APP_GPU] == (LATCH_APP, "mali-event-hnd")
        assert tasks[LATCH_SM] == (LATCH_SM, "surface-manager")
        outs = {"table": run_latch(log, table), "gz": run_latch(log, gz),
                "tids": run_latch(log, plain, app_gpu_tid=LATCH_APP_GPU, sm_gpu_tid=LATCH_SM_GPU),
                "none": run_latch(log, plain), "wrong": run_latch(log, table, tid=999)}
        # A log with no wait_at= is an error that names the trigger, not an empty report.
        with open(log, "w") as f:
            f.write("FRAMEDROP total=30.0 seq=1 mono=5.0\n")
        try:
            run_latch(log, plain)
        except SystemExit as e:
            assert "plxnative-framecb" in str(e)
        else:
            raise AssertionError("a log without wait_at= must refuse")
    assert outs["table"].replace(table, "T") == outs["gz"].replace(gz, "T"), \
        "plain and gzip thread tables must read the same"

    def has(key, pattern):
        assert re.search(pattern, outs[key]), f"{key}: missing /{pattern}/ in:\n{outs[key]}"

    # Owners from the thread table, and the same owners from explicit tids on a trace without one.
    for key, how in (("table", r"thread table, 4 threads"), ("tids", r"--app-gpu-tid/--sm-gpu-tid")):
        has(key, rf"mali irqs 23; job completions attributed \({how}\): app 11, compositor 11, other 1")
        # The four long waits, one per class, with their ticks; nothing else is listed.
        has(key, r"seq 3->4: wait +27\.3 +\+2 ticks +GPU-late +commit-aim +-\d+\.\d+ +gpu-aim +\+4\.20 ")
        has(key, r"seq 5->6: wait +\d+\.\d +\+2 ticks +compositor +commit-aim +-\d+\.\d+ +gpu-aim +-1\.50 ")
        has(key, r"seq 7->8: wait +15\.3 +\+2 ticks +CPU-late +commit-aim +\+0\.83 +gpu-aim +\+3\.80 ")
        has(key, r"seq 9->10: wait +19\.0 +\+1 tick +edge +commit-aim +-\d+\.\d+ +gpu-aim +\+0\.40 "
                 r".*\(draw runq 0\.31, swap runq 0\.07\)")
        assert outs[key].count("\n  seq ") == 4, outs[key]
        has(key, r"10 frame pairs scored; waits >= 10 ms: 4\n")
        for cls, ticks in (("CPU-late", r"\+2 ticks x1"), ("GPU-late", r"\+2 ticks x1"),
                           ("compositor", r"\+2 ticks x1"), ("edge", r"\+1 tick x1")):
            has(key, rf"\n  {cls} +1  \({ticks}\)")
        # The margin: the six on-time frames' GPU done minus aim, and minus their commit.
        has(key, r"on-time \(0-tick\) waited frames: n=6; GPU done minus aim p05/p50/p95/p99 "
                 r"-0\.50/\+0\.20/\+0\.40/\+0\.40 ms; GPU done minus commit p50 \+11\.37 ms")
    # No owners: the GPU columns are skipped with the reason, and only commit lateness can be told.
    has("none", r"mali irqs 23; GPU columns skipped: the trace has no thread table")
    has("none", r"seq 7->8: .*CPU-late")
    for seq in ("3->4", "5->6", "9->10"):
        has("none", rf"seq {seq}: .*unattributed +commit-aim.*gpu-aim +none")
    has("none", r"on-time \(0-tick\) waited frames: no GPU done times")
    # A --tid that is not in the table owns no jobs: say so instead of calling everything compositor-side.
    has("wrong", r"app 0, compositor 11, other 12")
    has("wrong", r"WARNING: no job completion belongs to the app")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("log", nargs="?")
    ap.add_argument("trace", nargs="*", help="one trace; several with --latch")
    ap.add_argument("--tid", type=int)
    ap.add_argument("--sm", default="surface-manager", help="comm prefix of the compositor's threads")
    ap.add_argument("--vsync-irq", type=int, default=-1, help="IRQ number of the vsync line (`osd_irq` is matched by name too)")
    ap.add_argument("--slow", type=float, default=20.0, help="report frames of at least this many ms")
    ap.add_argument("--offset-ms", type=float, default=0.0, help="trace clock minus CLOCK_MONOTONIC, if not mono")
    ap.add_argument("--latch", action="store_true",
                    help="classify each long back-buffer wait as CPU-late, GPU-late, edge or compositor-side")
    ap.add_argument("--wait-min", type=float, default=WAIT_MIN_MS, help="--latch: report waits of at least this many ms")
    ap.add_argument("--app-gpu-tid", type=int, help="--latch: tid of the app's mali-event-hnd (a trace with no thread table)")
    ap.add_argument("--sm-gpu-tid", type=int, help="--latch: tid of surface-manager's mali-event-hnd (same)")
    ap.add_argument("--self-test", action="store_true", help="run against a synthetic trace and exit")
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    if not (a.log and a.trace and a.tid):
        ap.error("APP.log, TRACE and --tid are required")
    if a.latch:
        return latch(a.log, a.trace, a)
    if len(a.trace) != 1:
        ap.error("give one TRACE (several are for --latch)")
    analyze(a.log, a.trace[0], a.tid, a.sm, a.vsync_irq, a.slow, a.offset_ms)


if __name__ == "__main__":
    main()
