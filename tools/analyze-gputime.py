#!/usr/bin/env python3
"""Summarize asynchronous GL_EXT_disjoint_timer_query JSONL without third-party packages."""

from __future__ import annotations

import argparse
import json
import statistics
from pathlib import Path


def percentile(values: list[int], fraction: float) -> float:
    ordered = sorted(values)
    at = (len(ordered) - 1) * fraction
    lo = int(at)
    hi = min(lo + 1, len(ordered) - 1)
    weight = at - lo
    return ordered[lo] * (1.0 - weight) + ordered[hi] * weight


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("jsonl", type=Path)
    parser.add_argument("--phase", help="include only this phase name")
    parser.add_argument("--discard", type=int, default=0, help="discard this many leading samples")
    args = parser.parse_args()

    rows = []
    disjoint_frames = set()
    with args.jsonl.open(encoding="utf-8") as source:
        for line in source:
            record = json.loads(line)
            if record.get("type") == "disjoint":
                disjoint_frames.add(record["frame"])
            elif record.get("type") == "timer" and (
                args.phase is None or record.get("name") == args.phase
            ):
                rows.append(record)

    # The app discards queries still PENDING when a disjoint is seen, but a query collected in the
    # same frame_end call was already recorded before the flag was read. Its interval overlaps the
    # disjoint window just as much, so drop it here — the frame stamps are in the file precisely so
    # this is decidable offline.
    suspect = disjoint_frames | {frame - 1 for frame in disjoint_frames}
    kept = [row for row in rows if row.get("collected_frame") not in suspect]
    dropped = len(rows) - len(kept)
    rows = kept[args.discard :]
    if not rows:
        raise SystemExit("no matching valid timer records")

    values = [row["gpu_ns"] for row in rows]
    to_ms = lambda value: value / 1_000_000.0
    names = sorted({row["name"] for row in rows})
    print(
        f"source={args.jsonl} samples={len(rows)} phases={','.join(names)} "
        f"disjoint_intervals={len(disjoint_frames)} dropped_near_disjoint={dropped}"
    )
    print(
        f"gpu_ms mean={to_ms(statistics.fmean(values)):.4f} "
        f"p50={to_ms(statistics.median(values)):.4f} "
        f"p95={to_ms(percentile(values, 0.95)):.4f} "
        f"p99={to_ms(percentile(values, 0.99)):.4f} max={to_ms(max(values)):.4f}"
    )


if __name__ == "__main__":
    main()
