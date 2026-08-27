#!/usr/bin/env python3
"""Grade the acquisition-transfer bound against real device logs.

WHAT IS BEING TESTED, and why it matters more than the number it produces.

`docs/adaptive-playback-spec.md` section 4 admits a candidate rung by comparing a predicted
acquisition time `A_j` against the media duration `D`. Predicting `A_j` for a rung the app has
never visited is the whole difficulty: the plan's first admission rule used the CATALOG rung rate
for it, scored 0.39 coverage against a nominal 0.90, and all fourteen misses were unsafe (finding
R1). The specification then owed an estimator for the two coefficients of

    A = O0 + bytes * tau

and section 8 listed that estimator as the first thing blocking implementation -- "nothing
downstream is implementable without it".

**This tool exists because that estimator is not needed.** Under the model above with the only two
constraints physics supplies -- a fixed cost cannot be negative (`O0 >= 0`) and more bytes cannot
cost less (`tau >= 0`) -- the acquisition time at any other byte count is bounded in closed form by
a SINGLE observation, with no coefficient estimated at all:

    A_j = A_i + (b_j - b_i) * tau
    b_j >= b_i:  tau <= A_i / b_i   (since O0 >= 0)  =>  A_j <= A_i * b_j / b_i
    b_j <  b_i:  tau >= 0                            =>  A_j <= A_i

    both cases:  A_j <= A_i * max(1, b_j / b_i)                          (the TRANSFER BOUND)

Both bounds are TIGHT: the first is attained at `O0 = 0`, the second at `tau = 0`. So the bound is
the exact worst case over every split of `A_i` between the two coefficients that the data cannot
distinguish -- which is precisely the split the corpus was shown to be unable to identify (R7: ten
effective degrees of freedom, `bytes` collinear with rung). The identification problem is dissolved
rather than solved.

Note the asymmetry, because it decides how much of the ladder needs a size prediction at all: a
DOWNSHIFT bound needs no `b_j`. Acquisition cannot rise when the byte count falls, so `A_j <= A_i`
holds whatever the candidate's size turns out to be. Only upshifts need `docs/measurements/
p2h-pms-ladder.md` section 2a's per-rung `sigma`, and the three rungs where `sigma` has no usable
ceiling (320, 720, 2000) are downshift targets.

THREE GRADES, and the first one FAILS on purpose.

1. `pairs`   -- the bound applied to a single observation, over every ordered pair in a case.
                Refuted: ~37% violations. The derivation above is deterministic and real
                acquisitions are not, so a single observation cannot carry an exceedance
                guarantee. Kept as a grade because it is the honest reason the next one exists,
                and because its FAILURE SHAPE is the evidence for the precondition: steady-link
                cases overshoot by 1.05-1.06 (noise around the model) while cases whose link
                changes mid-run overshoot by up to 20x (the "same link regime" precondition
                genuinely broken).
2. `order`    -- the shipped form. Over a trailing window of `n` observations, transfer each to the
                candidate byte count and take the `k`-th largest. Under exchangeability of the
                pairs `(b_i, A_i)`, the transferred values are a fixed measurable function of them
                and so are exchangeable too, giving `P(A_next > kth largest) = k/(n+1)` exactly.
                No coefficient, no fit, no floating point required.
3. `climb`   -- the question that killed two previous designs. A bound that never admits an
                upshift is useless however safe it is (the specification's own review: "section 5's
                trigger set contains no upshift condition -- climbing unreachable"). Reports, per
                case, the largest byte ratio the bound would still admit, by bisection.

The exchangeability argument covers the STATISTICS. It does not cover the MODEL step: `A_j <=
T_next` holds only if the plant parameters really are those implied by the next observation. That
assumption is named here rather than buried, and grade 1 is what it looks like when it breaks.

Reads the `hls: segment=` lines this project's event log already writes, so it needs no device and
no new instrumentation. Segment 0 is dropped everywhere: it carries decoder and encoder cold start
and is not a steady-state acquisition (`docs/measurements/p1-transaction-anatomy.md`).
"""

from __future__ import annotations

import argparse
import glob
import itertools
import os
import re
import statistics
import sys

SEGMENT_RE = re.compile(r"hls: segment=(\d+) bytes=(\d+).*?total_ms=(\d+)")

# Media seconds per segment, as the fixture tier serves them. Not a tunable: it is the segment
# duration the HLS playlists declare, and the admission rule compares acquisition against it.
DEFAULT_DURATION_MS = 2000


def read_segments(path: str) -> list[tuple[int, int]]:
    """`(bytes, acquisition_ms)` per steady-state segment, cold start excluded."""
    out = []
    with open(path, errors="replace") as handle:
        for line in handle:
            found = SEGMENT_RE.search(line)
            if found and int(found.group(1)) > 0:
                out.append((int(found.group(2)), int(found.group(3))))
    return out


def transfer(acquisition_ms: int, observed_bytes: int, query_bytes: float) -> float:
    """`A_i * max(1, b_j / b_i)` -- the worst case over every admissible `(O0, tau)` split."""
    if observed_bytes <= 0:
        return float(acquisition_ms)
    return acquisition_ms * max(1.0, query_bytes / observed_bytes)


def grade_pairs(segments: list[tuple[int, int]]) -> tuple[int, int, float]:
    """Single-observation bound over every ordered pair. Expected to fail; see the module doc."""
    total = violations = 0
    worst = 1.0
    for (b_i, a_i), (b_j, a_j) in itertools.permutations(segments, 2):
        total += 1
        bound = transfer(a_i, b_i, b_j)
        if a_j > bound:
            violations += 1
            worst = max(worst, a_j / bound)
    return total, violations, worst


def grade_order(
    segments: list[tuple[int, int]], window: int, k: int
) -> tuple[int, int, float]:
    """The shipped form: `k`-th largest transferred value over a trailing window of `window`."""
    total = exceedances = 0
    worst = 1.0
    for t in range(window, len(segments)):
        past = segments[t - window : t]
        b_j, a_j = segments[t]
        transferred = sorted(transfer(a_i, b_i, b_j) for b_i, a_i in past)
        bound = transferred[-k]
        total += 1
        if a_j > bound:
            exceedances += 1
            worst = max(worst, a_j / bound if bound else float("inf"))
    return total, exceedances, worst


def admissible_ratio(
    past: list[tuple[int, int]], current_bytes: int, k: int, duration_ms: int
) -> float:
    """Largest byte ratio whose transferred bound still fits inside one media duration.

    Bisected rather than solved: the bound is a `k`-th order statistic of a max of two linear
    pieces, so it is monotone in the ratio (every term is non-decreasing) but has no closed form.
    Monotonicity is what makes bisection valid here.
    """
    low, high = 1.0, 40.0
    for _ in range(40):
        mid = (low + high) / 2
        query = current_bytes * mid
        transferred = sorted(transfer(a_i, b_i, query) for b_i, a_i in past)
        if transferred[-k] <= duration_ms:
            low = mid
        else:
            high = mid
    return low


def grade_climb(
    segments: list[tuple[int, int]], window: int, k: int, duration_ms: int
) -> tuple[float, float] | None:
    """Median `A/D`, and the median byte ratio the bound would admit at that state."""
    ratios, loads = [], []
    for t in range(window, len(segments)):
        past = segments[t - window : t]
        current_bytes, acquisition = segments[t]
        loads.append(acquisition / duration_ms)
        ratios.append(admissible_ratio(past, current_bytes, k, duration_ms))
    if not ratios:
        return None
    return statistics.median(loads), statistics.median(ratios)


def cases(log_glob: str) -> list[tuple[str, list[tuple[int, int]]]]:
    found = []
    for path in sorted(glob.glob(log_glob)):
        segments = read_segments(path)
        if segments:
            found.append((os.path.basename(path).rsplit(".", 1)[0], segments))
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument(
        "--logs",
        default="docs/measurements/p1b-logs/*.log",
        help="glob for device event logs carrying `hls: segment=` lines",
    )
    parser.add_argument("--window", type=int, default=20, help="trailing window length n")
    parser.add_argument("--k", type=int, default=1, help="order statistic; eps = k/(n+1)")
    parser.add_argument(
        "--duration-ms", type=int, default=DEFAULT_DURATION_MS, help="media ms per segment"
    )
    parser.add_argument(
        "--grade",
        default="all",
        choices=["all", "pairs", "order", "climb", "sweep"],
        help="which grade to run",
    )
    args = parser.parse_args()

    found = cases(args.logs)
    if not found:
        print(f"no logs with `hls: segment=` lines matched {args.logs}", file=sys.stderr)
        return 2

    if args.grade in ("all", "pairs"):
        print("### 1. Single-observation transfer bound -- REFUTED, and the shape is the point\n")
        print(f"{'case':34s} {'pairs':>8} {'viol':>7} {'rate':>8} {'worst':>8}")
        totals = [0, 0]
        for name, segments in found:
            total, violations, worst = grade_pairs(segments)
            totals[0] += total
            totals[1] += violations
            print(f"{name:34s} {total:8d} {violations:7d} {violations/total:8.2%} {worst:8.2f}")
        print(f"\npooled: {totals[1]}/{totals[0]} = {totals[1]/totals[0]:.2%} violated\n")

    if args.grade in ("all", "order", "sweep"):
        print("### 2. Order-statistic transfer bound -- the shipped form\n")
        print(f"{'n':>4} {'k':>3} {'nominal eps':>12} {'observed':>10} {'tested':>8} {'worst':>7}")
        settings = (
            [(10, 1), (20, 1), (20, 2), (29, 1), (29, 3), (40, 2)]
            if args.grade == "sweep"
            else [(args.window, args.k)]
        )
        for window, k in settings:
            total = exceedances = 0
            worst = 1.0
            for _, segments in found:
                sub_total, sub_exceed, sub_worst = grade_order(segments, window, k)
                total += sub_total
                exceedances += sub_exceed
                worst = max(worst, sub_worst)
            observed = exceedances / total if total else 0.0
            print(
                f"{window:4d} {k:3d} {k/(window+1):12.3%} {observed:10.2%} {total:8d} {worst:7.2f}"
            )
        print()

    if args.grade in ("all", "climb"):
        print("### 3. Can it climb? Largest byte ratio the bound still admits\n")
        print(f"{'case':34s} {'median A/D':>11} {'median ratio admitted':>22}")
        for name, segments in found:
            result = grade_climb(segments, args.window, args.k, args.duration_ms)
            if result:
                load, ratio = result
                print(f"{name:34s} {load:11.2f} {ratio:22.2f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
