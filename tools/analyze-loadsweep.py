#!/usr/bin/env python3
"""Split one load-dial run into per-configuration distributions.

The dial (`ui::glassload`, `/tmp/plxnative-glassload`) cycles its steps inside a single launch and
stamps the live step onto the once/sec heartbeat as `load=<i>`, onto every FRAMEDROP line, and onto
every Mali HWCNT phase record as `"load":<i>`. This reader groups by that index so a cycled run
becomes an interleaved A/B/C/... — which is the only sound way to compare legs on a set that drifts
60 fps -> 50 fps over a session.

    tools/analyze-loadsweep.py run.log
    tools/analyze-loadsweep.py run.log --hwcnt pkg/plxnative-hwcnt.jsonl --phase frame.ui

Two samples are dropped at every step boundary by default (`--settle`): the heartbeat second that
straddles a rollover mixes two configurations, and the first refresh after one lands in the
expensive warm-up mode `docs/backdrop-blur-profiling.md` documents.
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
from pathlib import Path

FPS_RE = re.compile(r"\bloop=(\d+) route=(\w+).*?\bfps=(\d+) load=(-?\d+)(?: snap=(\d+))?")
DROP_RE = re.compile(
    r"FRAMEDROP total=([\d.]+) pump=([\d.]+) draw=([\d.]+) cap=([\d.]+) swap=([\d.]+).*?load=(-?\d+)"
)
STEP_RE = re.compile(r"GLASSLOAD step=(-?\d+)")
ARMED_RE = re.compile(r"GLASSLOAD armed .*")
CONFIG_RE = re.compile(r"PROFILE blur_config .*")


def pct(values, fraction):
    ordered = sorted(values)
    if not ordered:
        return 0.0
    at = (len(ordered) - 1) * fraction
    lo = int(at)
    hi = min(lo + 1, len(ordered) - 1)
    return ordered[lo] * (1.0 - (at - lo)) + ordered[hi] * (at - lo)


def summarize(name, groups, unit=""):
    """Per-step distribution, plus DRIFT — the last third's mean minus the first third's.

    Samples are appended in time order and the sweep cycles, so a step's list spans the whole run.
    Drift is therefore the direct test for a session that decays under sustained load (thermal, or
    anything else that gets worse the longer the set is on), which a median cannot show and which
    `tests/run.py` reports for the same reason.
    """
    print(f"\n== {name} by load step ==")
    print(f"{'step':>5} {'n':>5} {'median':>12} {'mean':>12} {'p10':>10} {'min':>10} {'max':>10} {'drift':>10}")
    for step in sorted(groups):
        v = groups[step]
        if not v:
            continue
        third = max(len(v) // 3, 1)
        drift = statistics.fmean(v[-third:]) - statistics.fmean(v[:third])
        print(
            f"{step:>5} {len(v):>5} {statistics.median(v):>12.1f} {statistics.fmean(v):>12.1f} "
            f"{pct(v, 0.10):>10.1f} {min(v):>10.1f} {max(v):>10.1f} {drift:>+10.2f}{unit}"
        )


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("log", type=Path, help="event log from the run")
    ap.add_argument("--hwcnt", type=Path, help="the run's plxnative-hwcnt.jsonl, if it had one")
    ap.add_argument("--phase", default="frame.ui", help="which HWCNT phase to group (default frame.ui)")
    ap.add_argument("--settle", type=int, default=2,
                    help="samples to drop after each step boundary (default 2)")
    args = ap.parse_args()

    text = args.log.read_text(errors="replace").splitlines()
    for line in text:
        if ARMED_RE.search(line):
            print(line.strip())
    seen_cfg = []
    for line in text:
        m = CONFIG_RE.search(line)
        if m and m.group(0) not in seen_cfg:
            seen_cfg.append(m.group(0))
    for c in seen_cfg:
        print(c)
    if len(seen_cfg) > 1:
        print(f"note: {len(seen_cfg)} distinct blur_config lines — regions changed across the run "
              "(expected for an area sweep; NOT expected within one step)")

    fps, drops, since, snaps, routes = {}, {}, {}, {}, {}
    cur = None
    for line in text:
        if (m := STEP_RE.search(line)) is not None:
            cur = int(m.group(1))
            since[cur] = 0
            continue
        if (m := FPS_RE.search(line)) is not None:
            step = int(m.group(4))
            since[step] = since.get(step, 0) + 1
            if since[step] <= args.settle:
                continue
            fps.setdefault(step, []).append(float(m.group(3)))
            routes.setdefault(step, set()).add(m.group(2))
            if m.group(5) is not None:
                # Blur refreshes ACTUALLY taken that second. Rule 7 of the methodology: check the
                # cadence rather than believing it — a leg that silently refreshed every frame once
                # read as an 11% regression of something unrelated.
                snaps.setdefault(step, []).append(float(m.group(5)))
        if (m := DROP_RE.search(line)) is not None:
            step = int(m.group(6))
            drops.setdefault(step, []).append(
                tuple(float(m.group(i)) for i in range(1, 6))
            )
    _ = cur
    summarize("fps= (frames actually swapped)", fps)
    if snaps:
        summarize("snap= (blur refreshes actually taken, per second)", snaps)
    if routes:
        print("\nroute seen per step: " + ", ".join(
            f"{k}={'/'.join(sorted(v))}" for k, v in sorted(routes.items())))

    if drops:
        print("\n== FRAMEDROP (frames over the threshold) by load step ==")
        print(f"{'step':>5} {'n':>5} {'med total':>10} {'pump':>8} {'draw':>8} {'cap':>8} {'swap':>8}")
        for step in sorted(drops):
            rows = drops[step]
            cols = list(zip(*rows))
            print(
                f"{step:>5} {len(rows):>5} {statistics.median(cols[0]):>10.1f} "
                f"{statistics.median(cols[1]):>8.1f} {statistics.median(cols[2]):>8.1f} "
                f"{statistics.median(cols[3]):>8.1f} {statistics.median(cols[4]):>8.1f}"
            )

    if args.hwcnt and args.hwcnt.exists():
        # GPU_ACTIVE is jm word 6 (see tools/analyze-hwcnt.py, which owns the counter table).
        cycles, tiles, quads, rbeats, wbeats = {}, {}, {}, {}, {}
        # The JSONL carries no step-boundary marker, but it IS in order, so a boundary is where the
        # `load` field changes. `--settle` samples are dropped after each one — a rollover
        # invalidates the snapshot, and the refresh that follows lands in the expensive warm-up
        # mode the profiling note documents.
        prev, held = None, 0
        drop_n = max(args.settle, 1) * 20
        with args.hwcnt.open(encoding="utf-8") as src:
            for line in src:
                rec = json.loads(line)
                if rec.get("type") != "phase" or rec.get("name") != args.phase:
                    continue
                step = int(rec.get("load", -1))
                held = held + 1 if step == prev else 0
                prev = step
                if held < drop_n:
                    continue
                w = rec["interval"]
                cycles.setdefault(step, []).append(float(w[6]))
                tiles.setdefault(step, []).append(float(w[3 * 64 + 20] + w[4 * 64 + 20]))
                quads.setdefault(step, []).append(float(w[3 * 64 + 14] + w[4 * 64 + 14]))
                # External MEMORY TRAFFIC: the candidate for what binds when cycles do not move.
                # l2 words 31/30, i.e. absolute indices 2*64+31 and 2*64+30.
                rbeats.setdefault(step, []).append(float(w[2 * 64 + 31]))
                wbeats.setdefault(step, []).append(float(w[2 * 64 + 30]))
        summarize(f"GPU_ACTIVE cycles, phase={args.phase}", cycles)
        summarize(f"FRAG_NUM_TILES, phase={args.phase}", tiles)
        summarize(f"FRAG_QUADS_RAST, phase={args.phase}", quads)
        summarize(f"L2_EXT_READ_BEATS, phase={args.phase}", rbeats)
        summarize(f"L2_EXT_WRITE_BEATS, phase={args.phase}", wbeats)


if __name__ == "__main__":
    main()
