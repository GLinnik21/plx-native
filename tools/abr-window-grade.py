#!/usr/bin/env python3
"""Grade the app's `abr: window` shadow against an independent reimplementation of the same rule.

**What this answers, and why it is not the same question `tools/abr-transfer-bound.py` answers.**
That tool grades the *rule* — how often a real acquisition exceeds the bound the rule promised, on
a corpus of captured segments. This one grades the *implementation*: given the exact segment stream
the app saw, does the integer arithmetic that ran on the television agree with the specification as
written down here, line for line? Those are independent failures. A correct rule computed wrongly
and a wrong rule computed correctly both produce a log full of plausible numbers, and neither is
visible from the other's side.

**Every input comes from the app's own lines, which is what makes the comparison exact.** `abr:
window` carries the query `bytes` and the segment duration; the `abr: sample` line emitted for the
same segment carries `prod`, from which the acquisition follows. The two are written adjacently by
one call site, so pairing them by order is not a heuristic.

**The one unavoidable imprecision is quantization, and it is handled as an interval rather than as a
tolerance.** `prod` is a truncated per-mille, so a logged `prod=p` at duration `D` means the
acquisition lay in `[p*D, p*D + D)` microseconds -- an interval this tool propagates through both
sums. A disagreement is reported only when the app's value falls OUTSIDE the interval the logs
admit. There is no epsilon anywhere in this file: a fudge factor here would hide exactly the
off-by-one the comparison exists to find.

Usage:
    tools/abr-window-grade.py docs/measurements/<inc>-logs/*.log
"""

from __future__ import annotations

import glob
import pathlib
import re
import sys

RE_SAMPLE = re.compile(
    r"abr: sample current=(\d+)kbps media=(\d+)kbps net=(\d+)kbps buf=(-?\d+)ms "
    r"vbuf=(-?\d+)ms abuf=(\S+) dur=(\d+)ms prod=(\d+)pm n=(\d+) decision=(\S+) "
    r"target=(\d+)kbps"
)
RE_WINDOW = re.compile(
    r"abr: window current=(\d+)kbps verdict=(\S+) have=(\d+)/(\d+) eps=(\d+)pm clamp=(\d+) "
    r"bound=(-?\d+)ms demand=(-?\d+)ms supply=(-?\d+)ms excess=(-?\d+)ms "
    r"sus=(\d+) sur=(\d+) reset=(\d+) bytes=(\d+) dur=(\d+)ms"
)

WINDOW_CAPACITY = 64


def transferred_us(bytes_i: int, acq_us: int, query: int) -> int:
    """`A_i * max(1, q/b_i)`, ceiled -- the shipped form, rewritten from the specification."""
    if query <= bytes_i:
        return acq_us
    b = max(bytes_i, 1)
    return -((-acq_us * query) // b)  # ceiling division, no float


def paired(lines):
    """(sample, window) per segment, in order.

    Paired by ADJACENCY because one call site writes both, in that order, for one segment. A
    `window` line with no `sample` before it means the two drifted apart and every number below
    would be attributed to the wrong segment, so it is an error rather than a skip.
    """
    out, pending = [], None
    for line in lines:
        m = RE_SAMPLE.search(line)
        if m:
            pending = {"buf_ms": int(m.group(4)), "dur_ms": int(m.group(7)),
                       "prod_pm": int(m.group(8))}
            continue
        m = RE_WINDOW.search(line)
        if not m:
            continue
        if pending is None:
            raise SystemExit("`abr: window` with no preceding `abr: sample`; the pairing is broken")
        out.append((pending, {
            "verdict": m.group(2), "have": int(m.group(3)), "want": int(m.group(4)),
            "eps_pm": int(m.group(5)), "clamp": int(m.group(6)),
            "bound_ms": int(m.group(7)), "demand_ms": int(m.group(8)),
            "supply_ms": int(m.group(9)), "excess_ms": int(m.group(10)),
            "sus": int(m.group(11)), "sur": int(m.group(12)),
            "resets": int(m.group(13)),
            "bytes": int(m.group(14)), "dur_ms": int(m.group(15)),
        }))
        pending = None
    return out


def acquisition_interval(sample) -> tuple[int, int]:
    """`[lo, hi)` microseconds admitted by a truncated `prod=` at this duration."""
    lo = sample["prod_pm"] * sample["dur_ms"]
    return lo, lo + sample["dur_ms"]


def grade(path: str):
    lines = pathlib.Path(path).read_text(errors="replace").splitlines()
    rows = paired(lines)
    if not rows:
        return None

    ring: list[tuple[int, int, int]] = []          # (bytes, acq_lo_us, acq_hi_us)
    checked = disagree = filling = resets = 0
    seen_resets = 0
    worst = ("", 0)
    for sample, window in rows:
        # **A reset is replayed from the app's own counter, never inferred from `have`.** The
        # window clears on a delivery regime change and the grader cannot see one -- a collapse is
        # not in these lines. Keying on the monotone counter means a legitimate reset costs
        # nothing, while a `have` that drops WITHOUT the counter moving is still caught, which is
        # the case worth catching.
        if window["resets"] > seen_resets:
            resets += window["resets"] - seen_resets
            seen_resets = window["resets"]
            ring = []
        elif window["resets"] < seen_resets:
            print(f"  ! reset={window['resets']} went BACKWARDS from {seen_resets}; not monotone")
            disagree += 1
            seen_resets = window["resets"]

        lo, hi = acquisition_interval(sample)
        if window["bytes"] and hi > 0:
            ring.append((window["bytes"], lo, hi))
            ring = ring[-WINDOW_CAPACITY:]

        n, q, d_us = window["want"], window["bytes"], window["dur_ms"] * 1_000
        have = min(len(ring), WINDOW_CAPACITY)
        if have != window["have"]:
            print(f"  ! have={window['have']} but this file's own segments give {have}")
            disagree += 1
            continue
        if have < n:
            if window["verdict"] != "filling":
                print(f"  ! have={have} < want={n} yet verdict={window['verdict']}")
                disagree += 1
            filling += 1
            continue

        recent = ring[-n:]
        d_lo = sum(transferred_us(b, a_lo, q) for b, a_lo, _ in recent)
        d_hi = sum(transferred_us(b, a_hi - 1, q) for b, _, a_hi in recent)
        e_lo = sum(max(transferred_us(b, a_lo, q) - d_us, 0) for b, a_lo, _ in recent)
        e_hi = sum(max(transferred_us(b, a_hi - 1, q) - d_us, 0) for b, _, a_hi in recent)
        supply = d_us * n

        checked += 1
        for name, got, (want_lo, want_hi) in (
            ("demand", window["demand_ms"], (d_lo // 1_000, d_hi // 1_000)),
            ("excess", window["excess_ms"], (e_lo // 1_000, e_hi // 1_000)),
        ):
            if not want_lo <= got <= want_hi:
                off = min(abs(got - want_lo), abs(got - want_hi))
                print(f"  ! {name}={got}ms outside the logged interval [{want_lo},{want_hi}]ms")
                disagree += 1
                if off > worst[1]:
                    worst = (f"{name} off by {off}ms", off)
        if window["supply_ms"] != supply // 1_000:
            print(f"  ! supply={window['supply_ms']}ms, n*D says {supply // 1_000}ms")
            disagree += 1
        # The two conditions, re-decided here. `sus`/`sur` are booleans the app printed; the
        # interval only makes them ambiguous when it straddles the boundary, which is reported
        # rather than silently accepted either way.
        if d_hi <= supply and not window["sus"]:
            print("  ! sus=0 but the whole interval is sustainable")
            disagree += 1
        if d_lo > supply and window["sus"]:
            print("  ! sus=1 but the whole interval is unsustainable")
            disagree += 1
    return {"rows": len(rows), "checked": checked, "filling": filling,
            "disagree": disagree, "resets": resets, "worst": worst[0]}


def occupancy(path: str):
    """What the shadow SAW, as a distribution -- the half a pass/fail cannot report.

    A run in which every graded line says `admit` with `demand` a fifth of `supply` has confirmed
    the arithmetic and characterised nothing, and that is worth knowing before the numbers are
    quoted as evidence about the rule.
    """
    rows = paired(pathlib.Path(path).read_text(errors="replace").splitlines())
    graded = [w for _, w in rows if w["verdict"] != "filling"]
    if not graded:
        return None
    loads = [w["demand_ms"] / w["supply_ms"] for w in graded if w["supply_ms"] > 0]
    verdicts = {v: sum(1 for w in graded if w["verdict"] == v) for v in ("admit", "refuse")}
    excess = [w["excess_ms"] for w in graded]
    return {
        "graded": len(graded), "verdicts": verdicts,
        "load_min": min(loads), "load_max": max(loads),
        "load_mean": sum(loads) / len(loads),
        "excess_max": max(excess), "excess_nonzero": sum(1 for e in excess if e > 0),
    }


def main(argv):
    paths = [p for a in argv for p in sorted(glob.glob(a))]
    if not paths:
        raise SystemExit(__doc__)
    print(f"{'case':<34} {'lines':>6} {'graded':>7} {'fill':>5} {'reset':>6} {'disagree':>9}")
    total_checked = total_disagree = 0
    occ = []
    for path in paths:
        r = grade(path)
        if r is None:
            continue
        name = pathlib.Path(path).stem[:34]
        print(f"{name:<34} {r['rows']:>6} {r['checked']:>7} {r['filling']:>5} "
              f"{r['resets']:>6} {r['disagree']:>9}")
        total_checked += r["checked"]
        total_disagree += r["disagree"]
        o = occupancy(path)
        if o:
            occ.append((name, o))

    print()
    print(f"{total_checked} graded lines, {total_disagree} disagreements with the specification")
    if occ:
        print()
        print("what the shadow actually saw (a confirmed arithmetic on an idle link proves little):")
        print(f"  {'case':<34} {'admit':>6} {'refuse':>7} {'load min':>9} {'mean':>7} "
              f"{'max':>7} {'exc>0':>6} {'exc max':>8}")
        for name, o in occ:
            print(f"  {name:<34} {o['verdicts']['admit']:>6} {o['verdicts']['refuse']:>7} "
                  f"{o['load_min']:>9.2f} {o['load_mean']:>7.2f} {o['load_max']:>7.2f} "
                  f"{o['excess_nonzero']:>6} {o['excess_max']:>7}ms")
    return 1 if total_disagree else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
