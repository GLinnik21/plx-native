#!/usr/bin/env python3
"""Measure what each ABR rung actually DELIVERS, from a real PMS, with no television.

This drives `pms-hls-probe.py` once per rung and pairs the resulting segment byte lists BY
MEDIA INDEX, which is the whole point: the same scene, encoded at two rungs, is the only
comparison in which a byte ratio is the actuator's effect rather than the content's.

## Why this is host-only work, when the plan filed it as a device job

The adaptive-playback plan ranks an "adjacent-pair byte census" among the cases needing a
45-minute television lease, because the byte sizes were assumed observable only as the app
fetched them. They are not. PMS hands the same bytes to any HTTP client, so the SIZE half of
the census needs no device at all -- only the TIMING half (transaction cost, queue behaviour,
the feed) does. Running it here costs minutes instead of a lease on a contended set.

## What it can and cannot answer

**Can:** what the server DECLARES per rung (`#EXT-X-STREAM-INF:BANDWIDTH`), what it actually
delivers (segment bytes), how those two relate, how the ratio between two rungs compares with
the ratio the ladder ASSUMES, and what the control plane and just-in-time production cost.

**Cannot: anything about the link.** Measured body throughput against the configured server is
reported for exactly this reason -- when it lands in the hundreds of Mbit/s the transport leg is
free, `tau` (per-byte transfer cost) is unmeasurable here, and any fit of one against this data
would be reading noise. The device tier owns that half.

**Cannot: whole-film statistics from one offset.** A run samples a contiguous span, and a film's
opening is atypical -- logos and titles encode small. Pass several `--offsets` before believing
any per-rung number is a property of the rung.
"""

import argparse
import json
import math
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PROBE = ROOT / "tools" / "pms-hls-probe.py"

# The shipped actuator ladder, read as (request kbps, raster) pairs. Kept beside the Rust rather
# than derived from it because this tool must be able to sweep a rung the ladder does NOT have --
# the question "is the spacing right" cannot be asked by a tool that can only ask about the
# current spacing. `--rungs` overrides it.
LADDER = [
    (320, "426x240"),
    (720, "854x480"),
    (2000, "1280x720"),
    (4000, "1280x720"),
    (6000, "1920x1080"),
    (8000, "1920x1080"),
    (10000, "1920x1080"),
    (12000, "1920x1080"),
    (14000, "1920x1080"),
    (16000, "1920x1080"),
    (18000, "1920x1080"),
    (20000, "1920x1080"),
    (22000, "3840x2160"),
]

SECONDS_PER_SEGMENT = 2


def rung_raster(kbps: int) -> str:
    """The raster the shipped ladder pairs with this request rate, or the top one above it."""
    for rung_kbps, raster in LADDER:
        if rung_kbps == kbps:
            return raster
    return LADDER[-1][1] if kbps > LADDER[-1][0] else LADDER[0][1]


def run_probe(item: str, kbps: int, raster: str, segments: int, offset: int, out: Path,
              owner: bool = False, python: str = sys.executable):
    """One probe run. Returns its report dict, or raises with the tool's own stderr attached."""
    out.mkdir(parents=True, exist_ok=True)
    argv = [
        python, str(PROBE),
        "--item", item,
        "--bitrate", str(kbps),
        "--resolution", raster,
        "--fixed-segments", str(segments),
        "--offset", str(offset),
        "--out", str(out),
    ]
    if owner:
        argv.append("--owner")
    done = subprocess.run(argv, capture_output=True, text=True)
    report = out / "report.json"
    if done.returncode != 0 or not report.exists():
        raise RuntimeError(
            f"probe failed for rung {kbps} (exit {done.returncode}): {done.stderr.strip()[:400]}"
        )
    return json.loads(report.read_text())


def segment_rows(report):
    """Per-segment (index, bytes, duration_s, ttfb_ms, body_ms) from one probe report.

    The duration is ffprobe's, not the playlist's `#EXTINF`, and not an assumed 2.0. A rate is
    bytes over TIME, so an assumed denominator would quietly convert a segmentation difference
    between two rungs into a bitrate difference -- the exact confound this tool exists to avoid.
    """
    rows = []
    for sample in report.get("segments") or []:
        if sample.get("status") != 200:
            continue
        timing = sample.get("timing") or {}
        probe = sample.get("probe") or {}
        raw = (probe.get("format") or {}).get("duration")
        try:
            duration = float(raw)
        except (TypeError, ValueError):
            duration = None
        rows.append({
            "index": sample.get("index"),
            "bytes": timing.get("bytes"),
            "duration_s": duration,
            "ttfb_ms": timing.get("ttfb_ms"),
            "body_ms": timing.get("body_ms"),
        })
    return rows


def declared_bandwidth(report):
    """The rate the server DECLARES for this rung, in bit/s, from the master playlist."""
    attributes = (report.get("start") or {}).get("variant_attributes") or []
    if not attributes:
        return None
    try:
        return int(attributes[0].get("BANDWIDTH"))
    except (TypeError, ValueError):
        return None


def declared_raster(report):
    attributes = (report.get("start") or {}).get("variant_attributes") or []
    return attributes[0].get("RESOLUTION") if attributes else None


def segment_rate_bps(row):
    """8 * bytes / duration, or None when either half is missing."""
    if not row.get("bytes") or not row.get("duration_s"):
        return None
    return row["bytes"] * 8.0 / row["duration_s"]


def pairable(left_rows, right_rows):
    """The indices present in BOTH rungs with a usable duration on each side.

    Pairing is by media index, so this is the guard that keeps a comparison honest: if two rungs
    were segmented differently -- a different `secondsPerSegment`, a different keyframe policy --
    the shared indices cover different media and the ratio means nothing. Callers must also check
    `duration_mismatch` below, because equal INDICES are not by themselves equal MEDIA.
    """
    left = {row["index"]: row for row in left_rows if segment_rate_bps(row)}
    right = {row["index"]: row for row in right_rows if segment_rate_bps(row)}
    return sorted(set(left) & set(right)), left, right


def duration_mismatch(indices, left, right, tolerance_s=0.05):
    """Indices whose two durations disagree by more than `tolerance_s`.

    Non-empty means the pairing is invalid at those indices and the caller must drop them rather
    than report a ratio. The tolerance is not a tuning knob: `#EXTINF` here is integer seconds
    while ffprobe reports the real PTS span, so two encodes of one span legitimately differ by
    a frame or two (at 24 fps, 42 ms). Anything past that is a different span, not rounding.
    """
    return [
        index for index in indices
        if abs(left[index]["duration_s"] - right[index]["duration_s"]) > tolerance_s
    ]


def ratio_stats(values):
    """min / median / max / geometric mean of a list of positive ratios.

    Geometric mean because these are RATIOS: the mean of 0.5x and 2.0x is 1.0x, not 1.25x.
    """
    clean = [value for value in values if value and value > 0]
    if not clean:
        return None
    return {
        "_values": clean,
        "n": len(clean),
        "min": min(clean),
        "median": statistics.median(clean),
        "max": max(clean),
        "geomean": math.exp(sum(math.log(value) for value in clean) / len(clean)),
        "spread": max(clean) / min(clean),
    }


def analyse(reports):
    """Turn `{request_kbps: report}` into the derived tables. Pure -- no I/O, no network."""
    rungs = []
    for kbps in sorted(reports):
        report = reports[kbps]
        rows = segment_rows(report)
        bandwidth = declared_bandwidth(report)
        rates = [rate for rate in (segment_rate_bps(row) for row in rows) if rate]
        # s = delivered segment rate / declared rate. Dimensionless, and comparable ACROSS rungs
        # in a way raw bytes are not -- which is what makes a pooled window over it legitimate.
        s_values = [rate / bandwidth for rate in rates] if bandwidth else []
        ttfb = [row["ttfb_ms"] for row in rows if row["ttfb_ms"] is not None]
        body = [row["body_ms"] for row in rows if row["body_ms"]]
        sizes = [row["bytes"] for row in rows if row["bytes"]]
        throughput = [
            row["bytes"] * 8.0 / (row["body_ms"] / 1000.0) / 1e6
            for row in rows if row["bytes"] and row["body_ms"]
        ]
        rungs.append({
            "request_kbps": kbps,
            "declared_kbps": bandwidth / 1000.0 if bandwidth else None,
            "declared_raster": declared_raster(report),
            "decided_kbps": ((report.get("decision") or {}).get("summary") or {}).get("bitrate"),
            "n": len(rows),
            "distinct_sizes": len(set(sizes)),
            "bytes": ratio_stats(sizes),
            "delivered_kbps": ratio_stats([rate / 1000.0 for rate in rates]),
            "s": ratio_stats(s_values),
            "ttfb_ms": ratio_stats(ttfb),
            "body_throughput_mbps": ratio_stats(throughput),
            "control_ms": {
                "decision": ((report.get("decision") or {}).get("timing") or {}).get("total_ms"),
                "master": ((report.get("start") or {}).get("timing") or {}).get("total_ms"),
                "media_playlist": [
                    (variant.get("timing") or {}).get("total_ms")
                    for variant in report.get("variants") or []
                ],
            },
            "child_count": (report.get("media") or {}).get("child_count"),
        })

    pairs = []
    ordered = sorted(reports)
    for left_kbps, right_kbps in zip(ordered, ordered[1:]):
        left_rows = segment_rows(reports[left_kbps])
        right_rows = segment_rows(reports[right_kbps])
        indices, left, right = pairable(left_rows, right_rows)
        dropped = duration_mismatch(indices, left, right)
        usable = [index for index in indices if index not in dropped]
        delivered = ratio_stats([
            segment_rate_bps(right[index]) / segment_rate_bps(left[index]) for index in usable
        ])
        left_bandwidth = declared_bandwidth(reports[left_kbps])
        right_bandwidth = declared_bandwidth(reports[right_kbps])
        pairs.append({
            "from_kbps": left_kbps,
            "to_kbps": right_kbps,
            "paired": len(usable),
            "dropped_for_duration": len(dropped),
            "catalog_ratio": right_kbps / left_kbps,
            "declared_ratio": (
                right_bandwidth / left_bandwidth if left_bandwidth and right_bandwidth else None
            ),
            "delivered_ratio": delivered,
            # B2, the RELATIVE bound: is the DECLARED ratio an upper bound on the DELIVERED one?
            # If it is, an admission rule can scale the bytes it just measured at the current
            # rung by a ratio the master playlist already gave it, and needs no census, no
            # catalog rate and no margin.
            "relative_bound_violations": sum(
                1 for index in usable
                if right_bandwidth and left_bandwidth
                and segment_rate_bps(right[index]) / segment_rate_bps(left[index])
                > right_bandwidth / left_bandwidth
            ) if (left_bandwidth and right_bandwidth) else None,
            "relative_bound_slack": ratio_stats([
                (segment_rate_bps(right[index]) / segment_rate_bps(left[index]))
                / (right_bandwidth / left_bandwidth)
                for index in usable
            ]) if (left_bandwidth and right_bandwidth) else None,
        })
    return {"rungs": rungs, "pairs": pairs}


def _cell(stats, key, fmt="{:.2f}"):
    if not stats or stats.get(key) is None:
        return "n/a"
    return fmt.format(stats[key])


def _number(value, fmt="{:.2f}"):
    return fmt.format(value) if value is not None else "n/a"


def render(analysis) -> str:
    """The markdown tables. Generated, never transcribed -- a table typed by hand out of a log
    that a later run overwrote is how five wrong numbers reached a published measurement doc."""
    lines = []
    lines.append("### Per rung: requested, declared, delivered\n")
    lines.append(
        "| request kbps | declared kbps | declared raster | n | distinct sizes "
        "| delivered kbps med | s min | s med | s max |"
    )
    lines.append("|---:|---:|---|---:|---:|---:|---:|---:|---:|")
    for rung in analysis["rungs"]:
        lines.append(
            f"| {rung['request_kbps']} "
            f"| {_number(rung['declared_kbps'], '{:.0f}')} "
            f"| {rung['declared_raster'] or 'n/a'} "
            f"| {rung['n']} | {rung['distinct_sizes']} "
            f"| {_cell(rung['delivered_kbps'], 'median', '{:.0f}')} "
            f"| {_cell(rung['s'], 'min', '{:.3f}')} "
            f"| {_cell(rung['s'], 'median', '{:.3f}')} "
            f"| {_cell(rung['s'], 'max', '{:.3f}')} |"
        )

    lines.append("")
    lines.append("### Adjacent pairs: what the ladder assumes vs what it delivers\n")
    lines.append(
        "| step | paired | catalog | declared | delivered geomean | delivered min "
        "| delivered max | catalog error |"
    )
    lines.append("|---|---:|---:|---:|---:|---:|---:|---:|")
    for pair in analysis["pairs"]:
        delivered = pair["delivered_ratio"]
        error = "n/a"
        if delivered and pair["catalog_ratio"]:
            error = f"{delivered['geomean'] / pair['catalog_ratio']:.2f}x"
        lines.append(
            f"| {pair['from_kbps']}->{pair['to_kbps']} | {pair['paired']} "
            f"| {_number(pair['catalog_ratio'])} "
            f"| {_number(pair['declared_ratio'])} "
            f"| {_cell(delivered, 'geomean')} "
            f"| {_cell(delivered, 'min')} "
            f"| {_cell(delivered, 'max')} | {error} |"
        )

    lines.append("")
    lines.append("### Cost: control plane and just-in-time production\n")
    lines.append(
        "| request kbps | decision ms | master ms | media playlist ms | ttfb min "
        "| ttfb med | ttfb max | body throughput med Mbit/s |"
    )
    lines.append("|---:|---:|---:|---:|---:|---:|---:|---:|")
    for rung in analysis["rungs"]:
        control = rung["control_ms"]
        playlist = control["media_playlist"]
        lines.append(
            f"| {rung['request_kbps']} "
            f"| {_number(control['decision'], '{:.1f}')} "
            f"| {_number(control['master'], '{:.1f}')} "
            f"| {_number(playlist[0] if playlist else None, '{:.1f}')} "
            f"| {_cell(rung['ttfb_ms'], 'min', '{:.1f}')} "
            f"| {_cell(rung['ttfb_ms'], 'median', '{:.1f}')} "
            f"| {_cell(rung['ttfb_ms'], 'max', '{:.1f}')} "
            f"| {_cell(rung['body_throughput_mbps'], 'median', '{:.0f}')} |"
        )

    lines.append("")
    lines.append("### Are the two candidate size bounds ever violated, and how loose are they\n")
    lines.append(
        "| bound | scope | n | violations | slack med | slack max |"
    )
    lines.append("|---|---|---:|---:|---:|---:|")
    for rung in analysis["rungs"]:
        s = rung["s"]
        if not s:
            continue
        # B1, the STRUCTURAL bound: RFC 8216 requires BANDWIDTH to be the PEAK segment rate, so
        # s > 1 is the server declaring less than it sends -- and would make the manifest useless
        # as a bound. Slack is s itself: at s = 0.25 the bound over-states the segment 4x.
        violations = sum(1 for value in s["_values"] if value > 1.0)
        lines.append(
            f"| B1 rate <= declared | rung {rung['request_kbps']} | {s['n']} | {violations} "
            f"| {s['median']:.3f} | {s['max']:.3f} |"
        )
    for pair in analysis["pairs"]:
        slack = pair["relative_bound_slack"]
        if not slack:
            continue
        lines.append(
            f"| B2 ratio <= declared ratio | {pair['from_kbps']}->{pair['to_kbps']} "
            f"| {slack['n']} | {pair['relative_bound_violations']} "
            f"| {slack['median']:.3f} | {slack['max']:.3f} |"
        )
    return "\n".join(lines) + "\n"


def segments_csv(reports) -> str:
    """Every observation the tables rest on, in one small file.

    The tables are a summary and summaries cannot be re-questioned. Publishing the rows beside
    them is what lets a later reader ask something this tool did not compute -- an
    autocorrelation, a different quantile, a per-index pairing -- without a second run against a
    server whose content and load have both moved on.
    """
    lines = ["request_kbps,declared_bps,index,bytes,duration_s,ttfb_ms,body_ms"]
    for kbps in sorted(reports):
        bandwidth = declared_bandwidth(reports[kbps])
        for row in segment_rows(reports[kbps]):
            lines.append(
                f"{kbps},{bandwidth if bandwidth is not None else ''},{row['index']},"
                f"{row['bytes']},{row['duration_s']},{row['ttfb_ms']},{row['body_ms']}"
            )
    return "\n".join(lines) + "\n"


def load_sweep(directory: Path):
    """`{request_kbps: report}` from a finished sweep's artifacts, for re-analysis.

    Every table this tool prints is a pure function of the saved `report.json` files, so a
    changed analysis never needs the server again. That matters more than convenience here: a
    re-run would be a DIFFERENT sample -- different scenes if the offset moved, a differently
    loaded encoder either way -- so re-probing to answer a new question about an old measurement
    silently swaps the evidence underneath the conclusion.
    """
    reports = {}
    for child in sorted(directory.glob("rung-*")):
        report = child / "report.json"
        if not report.exists():
            continue
        try:
            kbps = int(child.name.split("-", 1)[1])
        except (IndexError, ValueError):
            continue
        reports[kbps] = json.loads(report.read_text())
    return reports


def main():
    parser = argparse.ArgumentParser(
        description="Sweep the ABR ladder against a real PMS and pair the byte lists by index."
    )
    parser.add_argument("--item", default="movie_h264_ac3_1080p", help="overlay item key")
    parser.add_argument("--owner", action="store_true", help="use the owner token")
    parser.add_argument(
        "--rungs",
        default=",".join(str(kbps) for kbps, _ in LADDER),
        help="comma-separated request rates in kbps",
    )
    parser.add_argument("--segments", type=int, default=40, help="segments sampled per rung")
    parser.add_argument(
        "--offsets",
        default="0",
        help="comma-separated media offsets in seconds; each is a separate sweep",
    )
    parser.add_argument("--out", default=None, help="artifact directory outside the repository")
    parser.add_argument("--json", action="store_true", help="emit the analysis as JSON too")
    parser.add_argument(
        "--reanalyse",
        default=None,
        help="re-render the tables from a finished sweep's offset directory; touches no server",
    )
    args = parser.parse_args()

    if args.reanalyse:
        directory = Path(args.reanalyse)
        reports = load_sweep(directory)
        if not reports:
            parser.error(f"no rung-*/report.json under {directory}")
        analysis = analyse(reports)
        analysis["reanalysed_from"] = str(directory)
        text = render(analysis)
        (directory / "tables.md").write_text(text)
        (directory / "analysis.json").write_text(
            json.dumps(analysis, indent=2, sort_keys=True) + "\n"
        )
        (directory / "segments.csv").write_text(segments_csv(reports))
        print(text)
        if args.json:
            print(json.dumps(analysis, indent=2, sort_keys=True))
        return

    try:
        rungs = [int(value) for value in args.rungs.split(",") if value]
        offsets = [int(value) for value in args.offsets.split(",") if value != ""]
    except ValueError:
        parser.error("--rungs and --offsets take comma-separated integers")
    if not rungs or not offsets:
        parser.error("--rungs and --offsets must not be empty")
    if args.segments <= 0:
        parser.error("--segments must be positive")

    out = Path(args.out) if args.out else Path(tempfile.mkdtemp(prefix="pms-rung-sweep."))
    out.mkdir(parents=True, exist_ok=True)
    print(f"PMS rung sweep: {len(rungs)} rungs x {len(offsets)} offsets; artifacts={out}")

    for offset in offsets:
        reports = {}
        for kbps in rungs:
            raster = rung_raster(kbps)
            leg = out / f"offset-{offset}" / f"rung-{kbps}"
            try:
                reports[kbps] = run_probe(
                    args.item, kbps, raster, args.segments, offset, leg, args.owner
                )
            except RuntimeError as error:
                print(f"  rung {kbps}: {error}")
                continue
            rows = segment_rows(reports[kbps])
            bandwidth = declared_bandwidth(reports[kbps])
            print(
                f"  rung {kbps:>5} -> declared {bandwidth / 1000 if bandwidth else 0:.0f} kbps, "
                f"{len(rows)} segments, {len({row['bytes'] for row in rows})} distinct sizes"
            )
        if not reports:
            print(f"  offset {offset}: no rung completed")
            continue
        analysis = analyse(reports)
        analysis["offset_seconds"] = offset
        analysis["item_key"] = args.item
        analysis["segments_requested"] = args.segments
        text = render(analysis)
        (out / f"offset-{offset}" / "tables.md").write_text(text)
        (out / f"offset-{offset}" / "analysis.json").write_text(
            json.dumps(analysis, indent=2, sort_keys=True) + "\n"
        )
        (out / f"offset-{offset}" / "segments.csv").write_text(segments_csv(reports))
        print(f"\n## offset {offset} s\n")
        print(text)
        if args.json:
            print(json.dumps(analysis, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
