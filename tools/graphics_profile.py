"""Shared parsing and reporting for PlxNative's opt-in three-layer graphics profile."""

from __future__ import annotations

import json
import re
import statistics
from pathlib import Path


SAMPLE_RE = re.compile(r"^@@sample\s+(\d+)\s+(\d+)$")
HEARTBEAT_RE = re.compile(r"\bloop=(\d+)\s+route=(\w+).*?\bfps=(\d+)")


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    at = (len(ordered) - 1) * fraction
    lo = int(at)
    hi = min(lo + 1, len(ordered) - 1)
    weight = at - lo
    return ordered[lo] * (1.0 - weight) + ordered[hi] * weight


def parse_irq_snapshots(text: str, leg: str) -> list[dict]:
    """Normalize the helper's marked /proc/interrupts snapshots.

    CPU columns are the consecutive decimal words immediately after the IRQ's colon. Everything
    after them is retained as the kernel label instead of guessing a fixed GIC column layout.
    """
    rows = []
    sample = None
    stamp = None
    for raw in text.splitlines():
        if match := SAMPLE_RE.match(raw.strip()):
            sample, stamp = int(match.group(1)), int(match.group(2))
            continue
        if sample is None or ":" not in raw:
            continue
        irq, rest = raw.split(":", 1)
        tokens = rest.split()
        counts = []
        while tokens and tokens[0].isdigit():
            counts.append(int(tokens.pop(0)))
        if not counts:
            continue
        rows.append({
            "type": "irq",
            "leg": leg,
            "sample": sample,
            "monotonic_ns": stamp,
            "irq": irq.strip(),
            "name": " ".join(tokens),
            "counts": counts,
            "total": sum(counts),
        })
    return rows


def irq_rates(rows: list[dict], discard_samples: int = 0) -> list[dict]:
    previous: dict[str, dict] = {}
    rates = []
    for row in sorted(rows, key=lambda r: (r["sample"], r["irq"])):
        old = previous.get(row["irq"])
        previous[row["irq"]] = row
        if row["sample"] < discard_samples or old is None:
            continue
        elapsed = (row["monotonic_ns"] - old["monotonic_ns"]) / 1_000_000_000.0
        delta = row["total"] - old["total"]
        if elapsed <= 0 or delta < 0:
            continue
        rates.append({
            "leg": row["leg"],
            "sample": row["sample"],
            "irq": row["irq"],
            "name": row["name"],
            "per_second": delta / elapsed,
        })
    return rates


def summarize_irq(rows: list[dict], discard_samples: int = 0) -> dict:
    rates = irq_rates(rows, discard_samples)
    by_irq: dict[str, list[float]] = {}
    names = {}
    by_sample: dict[int, float] = {}
    for row in rates:
        by_irq.setdefault(row["irq"], []).append(row["per_second"])
        names[row["irq"]] = row["name"]
        by_sample[row["sample"]] = by_sample.get(row["sample"], 0.0) + row["per_second"]
    total = list(by_sample.values())
    return {
        "n": len(total),
        "total_mean": statistics.fmean(total) if total else 0.0,
        "total_p50": statistics.median(total) if total else 0.0,
        "total_p95": percentile(total, 0.95),
        "per_irq": {
            irq: {
                "name": names[irq],
                "mean": statistics.fmean(values),
                "p50": statistics.median(values),
                "p95": percentile(values, 0.95),
            }
            for irq, values in sorted(by_irq.items())
        },
    }


def summarize_pacing(lines: list[str], route: str | None = None) -> dict:
    rows = []
    for line in lines:
        match = HEARTBEAT_RE.search(line)
        if match and (route is None or match.group(2) == route):
            rows.append((int(match.group(1)), match.group(2), int(match.group(3))))
    fps = [float(row[2]) for row in rows]
    loops = [float(row[0]) for row in rows]
    third = max(len(fps) // 3, 1) if fps else 1
    return {
        "n": len(fps),
        "route": route or (rows[-1][1] if rows else ""),
        "fps_mean": statistics.fmean(fps) if fps else 0.0,
        "fps_p50": statistics.median(fps) if fps else 0.0,
        "fps_p10": percentile(fps, 0.10),
        "fps_p95": percentile(fps, 0.95),
        "fps_min": min(fps) if fps else 0.0,
        "fps_max": max(fps) if fps else 0.0,
        "fps_drift": statistics.fmean(fps[-third:]) - statistics.fmean(fps[:third]) if fps else 0.0,
        "loop_p50": statistics.median(loops) if loops else 0.0,
    }


def write_irq_jsonl(path: Path, groups: list[list[dict]]) -> None:
    with path.open("w", encoding="utf-8") as out:
        for rows in groups:
            for row in rows:
                out.write(json.dumps(row, separators=(",", ":")) + "\n")


def format_irq(name: str, summary: dict) -> list[str]:
    lines = [
        f"{name}: total irq/s mean={summary['total_mean']:.1f} "
        f"p50={summary['total_p50']:.1f} p95={summary['total_p95']:.1f} n={summary['n']}"
    ]
    for irq, values in summary["per_irq"].items():
        lines.append(
            f"  irq {irq}: mean={values['mean']:.1f} p50={values['p50']:.1f} "
            f"p95={values['p95']:.1f}  {values['name']}"
        )
    return lines


def selftest() -> None:
    raw = """@@sample 0 1000000000
125: 10 20 0 GIC mali-job
126: 40 0 0 GIC GPU MMU
@@sample 1 1100000000
125: 12 23 0 GIC mali-job
126: 44 1 0 GIC GPU MMU
@@sample 2 1200000000
125: 15 25 0 GIC mali-job
126: 50 1 0 GIC GPU MMU
"""
    rows = parse_irq_snapshots(raw, "production")
    assert len(rows) == 6
    summary = summarize_irq(rows)
    assert summary["n"] == 2
    assert round(summary["total_mean"]) == 105
    assert round(summary["per_irq"]["125"]["mean"]) == 50
    pacing = summarize_pacing([
        "[1] loop=62 route=home fps=58",
        "[2] loop=61 route=home fps=60",
    ], "home")
    assert pacing["fps_p50"] == 59


if __name__ == "__main__":
    selftest()
    print("graphics_profile selftest: ok")
