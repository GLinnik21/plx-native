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

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "tests"))
from run import RE_ABR_SAMPLE as RE_SAMPLE, RE_ABR_WINDOW as RE_WINDOW  # noqa: E402

# **The two log patterns come from `tests/run.py`, which owns them.** They were copied here
# character-for-character, including the `buf=` note, and a copy sits outside the contract test in
# `tests/test_harness.py` that pins them against the Rust format strings. A field added to
# `abr: sample` would redden that test and leave THIS file matching zero lines -- and zero matches
# print as "0 segment(s) compared, 0 disagreements", which is what a clean run looks like.
# Verified identical before the swap: over the 81 logs under `docs/measurements/`, the local
# copies and run.py's matched the same 4627 samples and the same 2658 window lines.

# `graded=` is the acquisition of a candidate transaction's graded segment and `graded_bytes=` its
# size -- together, the ONE observation `Controller::observe_candidate` adds to the window that no
# `abr: window` line describes. A replayer without them counts one short after every transaction
# that got that far, which reads as the app miscounting and is not.
RE_TX_GRADED = re.compile(r"abr: tx \S+ .*? graded=(\d+)ms .*? graded_bytes=(\d+)")

WINDOW_CAPACITY = 64


def transferred_us(bytes_i: int, acq_us: int, query: int) -> int:
    """`A_i * max(1, q/b_i)`, ceiled -- the shipped form, rewritten from the specification."""
    if query <= bytes_i:
        return acq_us
    b = max(bytes_i, 1)
    return -((-acq_us * query) // b)  # ceiling division, no float


#: A sample that was seen but carries no gradeable reserve. A distinct object rather than `None`
#: so that "no sample" (a real pairing break, which is an error) stays distinguishable from "a
#: sample whose reserve was unknowable" (a stated skip).
UNGRADEABLE = object()


def paired(lines):
    """The window's input stream, in order: `("candidate", bytes, acq_us)` or `("segment", …)`.

    Segments are paired by ADJACENCY because one call site writes `abr: sample` and `abr: window`
    in that order for one segment. A `window` line with no `sample` before it means the two drifted
    apart and every number below would be attributed to the wrong segment, so it is an error rather
    than a skip.

    A candidate observation is spliced in at its `abr: tx` line. That placement is exact rather than
    approximate: the transaction runs inline on the demux worker, so no current-stream segment is
    acquired while it is in flight, and the `tx` record therefore falls between the `abr: window`
    before the transaction and the one after it.
    """
    out, pending, ungradeable = [], None, 0
    for line in lines:
        m = RE_TX_GRADED.search(line)
        if m:
            out.append(("candidate", int(m.group(2)), int(m.group(1)) * 1_000))
            continue
        m = RE_SAMPLE.search(line)
        if m:
            # A `buf=none` sample cannot be graded against condition (2) — the reserve IS the
            # quantity that condition is about — so it pairs as `UNGRADEABLE` rather than with a
            # fabricated zero, which would score every such segment as an unsurvivable window.
            # It is still a pairing: the app emits the `abr: window` line for these segments too
            # (the readout sits above every early return), so dropping the sample entirely would
            # leave that line looking like the drift this function refuses to guess through.
            raw_buf = m.group(4)
            pending = UNGRADEABLE if raw_buf == "none" else {
                "buf_ms": int(raw_buf.rstrip("ms")), "dur_ms": int(m.group(7)),
                "prod_pm": int(m.group(8)),
            }
            continue
        m = RE_WINDOW.search(line)
        if not m:
            continue
        if pending is None:
            raise SystemExit("`abr: window` with no preceding `abr: sample`; the pairing is broken")
        if pending is UNGRADEABLE:
            pending = None
            ungradeable += 1
            continue
        out.append(("segment", pending, {
            "verdict": m.group(2), "have": int(m.group(3)), "want": int(m.group(4)),
            "eps_pm": int(m.group(5)), "clamp": int(m.group(6)),
            "bound_ms": int(m.group(7)), "demand_ms": int(m.group(8)),
            "supply_ms": int(m.group(9)), "excess_ms": int(m.group(10)),
            "sus": int(m.group(11)), "sur": int(m.group(12)),
            "resets": int(m.group(13)),
            "bytes": int(m.group(14)), "dur_ms": int(m.group(15)),
        }))
        pending = None
    if ungradeable:
        # Stated, never silent: a skipped segment is coverage this grading does not have, and a
        # count printed beside the verdict is the difference between "condition (2) held" and
        # "condition (2) held on the segments we could evaluate".
        print(f"note: {ungradeable} segment(s) skipped — reserve was `none` (audio lane silent)",
              file=sys.stderr)
    return out


def acquisition_interval(sample) -> tuple[int, int]:
    """`[lo, hi)` microseconds admitted by a truncated `prod=` at this duration."""
    lo = sample["prod_pm"] * sample["dur_ms"]
    return lo, lo + sample["dur_ms"]


def grade(rows):
    """`rows` come from `paired`, parsed ONCE by the caller.

    It used to take a path and parse the file itself, and so did `occupancy` -- so every log was
    read and paired twice, and `paired`'s own `note: N segment(s) skipped` printed twice per file,
    which reads as two separate skips.
    """
    if not rows:
        return None

    ring: list[tuple[int, int, int]] = []          # (bytes, acq_lo_us, acq_hi_us)
    checked = disagree = filling = resets = candidates = 0
    seen_resets = 0
    for row in rows:
        if row[0] == "candidate":
            # `graded=` is whole milliseconds, so its interval is [ms, ms+1) in microseconds --
            # narrower than a segment's, and propagated the same way rather than assumed exact.
            _, cand_bytes, acq_us = row
            if cand_bytes and acq_us:
                ring.append((cand_bytes, acq_us, acq_us + 1_000))
                ring = ring[-WINDOW_CAPACITY:]
                candidates += 1
            continue
        _, sample, window = row
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
                print(f"  ! {name}={got}ms outside the logged interval [{want_lo},{want_hi}]ms")
                disagree += 1
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
            "disagree": disagree, "resets": resets, "candidates": candidates}


def occupancy(rows):
    """What the shadow SAW, as a distribution -- the half a pass/fail cannot report.

    A run in which every graded line says `admit` with `demand` a fifth of `supply` has confirmed
    the arithmetic and characterised nothing, and that is worth knowing before the numbers are
    quoted as evidence about the rule.
    """
    graded = [r[2] for r in rows if r[0] == "segment" and r[2]["verdict"] != "filling"]
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
    print(f"{'case':<34} {'lines':>6} {'graded':>7} {'fill':>5} {'cand':>5} {'reset':>6} "
          f"{'disagree':>9}")
    total_checked = total_disagree = 0
    occ = []
    for path in paths:
        rows = paired(pathlib.Path(path).read_text(errors="replace").splitlines())
        r = grade(rows)
        if r is None:
            continue
        name = pathlib.Path(path).stem[:34]
        print(f"{name:<34} {r['rows']:>6} {r['checked']:>7} {r['filling']:>5} "
              f"{r['candidates']:>5} {r['resets']:>6} {r['disagree']:>9}")
        total_checked += r["checked"]
        total_disagree += r["disagree"]
        o = occupancy(rows)
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
