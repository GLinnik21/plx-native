#!/usr/bin/env python3
"""Summarize PlxNative's raw Mali HWCNT JSONL without third-party packages."""

from __future__ import annotations

import argparse
import json
import statistics
from pathlib import Path

BLOCK_WORDS = 64

# Reviewed subset of Arm's r12p0 Mali-T82x table. Layout-v5 order on this MP2 target is JM,
# tiler, one MMU/L2 slice, then the two shader cores. Unknown words remain addressable by raw index.
SPECS = (
    ("GPU_ACTIVE", "jm", 6),
    ("JS0_ACTIVE", "jm", 10),
    ("JS1_ACTIVE", "jm", 18),
    ("TILER_ACTIVE", "tiler", 22),
    ("FRAG_ACTIVE", "shader", 4),
    ("FRAG_PRIMITIVES", "shader", 5),
    ("FRAG_QUADS_RAST", "shader", 14),
    ("FRAG_NUM_TILES", "shader", 20),
    ("FRAG_TRANS_ELIM", "shader", 21),
    ("TRIPIPE_ACTIVE", "shader", 26),
    ("ARITH_WORDS", "shader", 27),
    ("LS_WORDS", "shader", 31),
    ("LS_ISSUES", "shader", 32),
    ("TEX_WORDS", "shader", 38),
    ("TEX_ISSUES", "shader", 42),
    ("LSC_READ_OP", "shader", 49),
    ("LSC_WRITE_OP", "shader", 51),
    ("SHADER_AXI_BEATS_READ", "shader", 62),
    ("SHADER_AXI_BEATS_WRITTEN", "shader", 63),
    ("MMU_REQUESTS", "l2", 9),
    ("L2_EXT_WRITE_BEATS", "l2", 30),
    ("L2_EXT_READ_BEATS", "l2", 31),
    ("L2_ANY_LOOKUP", "l2", 32),
    ("L2_READ_LOOKUP", "l2", 33),
    ("L2_READ_HIT", "l2", 37),
    ("L2_WRITE_LOOKUP", "l2", 39),
    ("L2_WRITE_HIT", "l2", 43),
    ("L2_EXT_READ", "l2", 48),
    ("L2_EXT_WRITE", "l2", 50),
    ("L2_EXT_W_STALL", "l2", 58),
)


def percentile(values: list[float | int], fraction: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    at = (len(ordered) - 1) * fraction
    lo = int(at)
    hi = min(lo + 1, len(ordered) - 1)
    weight = at - lo
    return float(ordered[lo]) * (1.0 - weight) + float(ordered[hi]) * weight


def decode(words: list[int], block: str, word: int) -> int:
    base = {"jm": 0, "tiler": 1, "l2": 2}
    if block == "shader":
        return words[3 * BLOCK_WORDS + word] + words[4 * BLOCK_WORDS + word]
    return words[base[block] * BLOCK_WORDS + word]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("jsonl", type=Path)
    parser.add_argument("--phase", help="include only this phase name")
    parser.add_argument("--discard", type=int, default=0, help="discard this many leading samples")
    parser.add_argument("--raw-top", type=int, default=12, help="show N busiest unnamed/raw words")
    args = parser.parse_args()

    info = None
    rows = []
    with args.jsonl.open(encoding="utf-8") as source:
        for line in source:
            record = json.loads(line)
            if record.get("type") == "info":
                info = record
            elif record.get("type") == "phase" and (
                args.phase is None or record.get("name") == args.phase
            ):
                rows.append(record)
    rows = rows[args.discard :]
    if not rows:
        raise SystemExit("no matching phase records")

    names = sorted({row["name"] for row in rows})
    print(f"source={args.jsonl} samples={len(rows)} phases={','.join(names)}")
    if info:
        print(
            "reader "
            + " ".join(
                f"{key}={info[key]}"
                for key in ("api", "hwver", "dump_size", "buffer_count", "map_size", "page_size")
            )
        )

    walls_ms = [row["serialized_wall_ns"] / 1_000_000.0 for row in rows]
    print(
        f"serialized_wall_ms(calibration_only) mean={statistics.fmean(walls_ms):.4f} "
        f"p50={statistics.median(walls_ms):.4f} p95={percentile(walls_ms, 0.95):.4f} "
        f"p99={percentile(walls_ms, 0.99):.4f} max={max(walls_ms):.4f}"
    )
    print("counter,mean,p50,p95,max")
    for name, block, word in SPECS:
        values = [decode(row["interval"], block, word) for row in rows]
        print(
            f"{name},{statistics.fmean(values):.2f},{statistics.median(values):.2f},"
            f"{percentile(values, 0.95):.2f},{max(values)}"
        )

    # Header words 0..3 are masks/metadata, not counters. This view keeps unnamed activity visible
    # without assigning it a speculative label.
    raw = []
    for index in range(len(rows[0]["interval"])):
        if index % BLOCK_WORDS < 4:
            continue
        values = [row["interval"][index] for row in rows]
        raw.append((statistics.fmean(values), index, statistics.median(values), max(values)))
    raw.sort(reverse=True)
    print("raw_top index,mean,p50,max")
    for mean, index, median, maximum in raw[: max(args.raw_top, 0)]:
        print(f"{index},{mean:.2f},{median:.2f},{maximum}")


if __name__ == "__main__":
    main()
