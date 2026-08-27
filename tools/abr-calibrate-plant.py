#!/usr/bin/env python3
"""Derive `rust-modules/src/abr/sim.rs`'s plant calibration from committed evidence.

**Why this is a tool and not a table somebody typed.** `sim.rs` is the closed-loop plant the ABR
controller is graded against, and it carried three hand-transcribed operating points. Two of the
three describe a fixture pack that no longer exists: the pack was rebuilt between the `p1` and `p1b`
capture runs, and at rung 720 the delivered rate moved 1381 -> 806 kbps (1.72x) while at rung 4000
it moved the other way. Nothing failed when that happened, because nothing recomputes the table. So
the table is now generated, its provenance is printed beside every number, and a stale one is a
`make check` failure rather than a silently wrong simulation.

**Two things are calibrated here and the second is the one that blocks the simulator.**

1. **Operating points** — `ts_kbps`, `audio_es_kbps`, `overhead_ms` per rung, from the M4 pin
   census. `sim.rs` refuses to run an uncalibrated rung rather than interpolating, so a
   three-point table confines every closed-loop experiment to three rungs of a thirteen-rung ladder.

2. **Transaction legs** — up/down x commit/reject. Every one is an `Option` in `sim.rs` and `run()`
   REFUSES the moment it needs a missing one, so **an uncalibrated transaction model means the
   simulator cannot execute any trace that changes rung at all**. Three of the four are now
   measured across the committed corpus. The fourth, `down_reject`, has n = 0 and that is
   structural rather than an oversight: `Controller::candidate_ready` accepts every downshift that
   produced a decodable segment, so a down-reject needs a decode or raster failure to happen at
   all. It is reported as absent, never invented.

Sources, all already in the repository:

* `abr: sample current= media= net= buf= dur= prod=` in `docs/measurements/*-logs/pipe_abr_pin_*.log`
  — the delivered TS rate, the reserve, and (as `prod*dur - media*dur/net`) the non-transfer part of
  acquisition.
* `abr: tx <dir> ... outcome= control= warmup= graded=` in every captured log — the transaction legs.
* `ffprobe` on the local fixture pack — the audio elementary rate, which the logs do not carry.

Usage:
    tools/abr-calibrate-plant.py                          # table + provenance
    tools/abr-calibrate-plant.py --rust                   # the Rust match arms
    tools/abr-calibrate-plant.py --fixtures <dir>         # non-default fixture pack
"""

from __future__ import annotations

import argparse
import collections
import glob
import json
import os
import pathlib
import re
import statistics
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_FIXTURES = pathlib.Path(os.path.expanduser("~/plxnative-fixtures/pipeline"))

RE_SAMPLE = re.compile(
    # `buf=` is `<n>ms` or the literal `none` -- the app cannot know the playable reserve on a
    # segment whose audio lane has produced no timestamp yet, and prints `none` rather than a
    # zero that reads as an empty buffer. Matched permissively HERE and rejected explicitly at
    # the use site, so such a sample is a stated skip rather than a line that silently stops
    # matching and disappears from the census.
    r"abr: sample current=(\d+)kbps media=(\d+)kbps net=(\d+)kbps buf=(\S+) "
    r"vbuf=(-?\d+)ms abuf=(\S+) dur=(\d+)ms prod=(\d+)pm"
)
RE_TX = re.compile(
    r"abr: tx (\w+) (\d+)->(\d+)kbps outcome=(\S+) decided=(-?\d+|none)ms total=(-?\d+)ms "
    r"control=(-?\d+|none)ms prime=(-?\d+|none)ms master=(-?\d+|none)ms media=(-?\d+|none)ms "
    r"warmup=(-?\d+|none)ms graded=(-?\d+|none)ms"
)

# `sim.rs`'s own documented assumption, repeated here so the two can be compared rather than
# silently diverge: the AU queues hold demuxed ELEMENTARY bytes while `media=` is measured off the
# TS wire. 1.04 is an assumed transport-stream overhead -- an assumption, not a measurement, and
# `sim.rs` says so where it uses it.
TS_OVERHEAD = 1.04

# Rung -> local fixture, mirroring `tests/serve_fixtures.py`'s ABR_FIXTURE. Read from that file at
# run time rather than copied, so a fixture rename cannot leave this silently pointing at the wrong
# clip -- which is the exact failure mode that produced the stale table this tool replaces.
def fixture_map() -> dict[str, str]:
    text = (ROOT / "tests" / "serve_fixtures.py").read_text()
    m = re.search(r"ABR_FIXTURE\s*=\s*\{(.*?)\n\}", text, re.S)
    if not m:
        raise SystemExit("ABR_FIXTURE not found in tests/serve_fixtures.py")
    return dict(re.findall(r'"(\d+)":\s*"([^"]+)"', m.group(1)))


def audio_es_kbps(path: pathlib.Path) -> int | None:
    """The audio elementary rate, which no log line carries."""
    if not path.exists():
        return None
    try:
        out = subprocess.run(
            ["ffprobe", "-v", "error", "-select_streams", "a:0",
             "-show_entries", "stream=bit_rate", "-of", "json", str(path)],
            capture_output=True, text=True, timeout=30, check=True).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    streams = json.loads(out).get("streams") or []
    if not streams:
        return None
    raw = streams[0].get("bit_rate")
    return round(int(raw) / 1000) if raw and raw != "N/A" else None


def pin_samples(path: pathlib.Path, rung: int):
    """Samples taken while the controller was actually ON the pinned rung."""
    rows = []
    for m in RE_SAMPLE.finditer(path.read_text(errors="replace")):
        cur, media, net, dur, prod = (int(m.group(i)) for i in (1, 2, 3, 7, 8))
        # A `buf=none` sample has no reserve to census. It is skipped rather than coerced,
        # because `buf_median_ms` is the plant's starting reserve and a zero would drag it down
        # by exactly the count of segments whose audio lane happened to be quiet.
        raw_buf = m.group(4)
        if raw_buf == "none":
            continue
        buf = int(raw_buf.rstrip("ms"))
        if cur != rung or not (media and net and dur):
            continue
        # `overhead` is the non-transfer part of acquisition, exactly as `sim.rs` defines it:
        # A - active, where active is the body transfer implied by the delivered rate.
        acquire_ms = prod * dur / 1000.0
        active_ms = media * dur / net
        rows.append({"media": media, "buf": buf, "dur": dur,
                     "overhead": max(0.0, acquire_ms - active_ms)})
    return rows


def settled(rows):
    """Drop the first quarter as queue fill-in — the M4 census's own convention.

    The reserve climbs monotonically from zero at the start of a pin, so a median over the whole run
    measures the fill-in as much as the ceiling. `media` and `overhead` do not need this, but they
    are taken over the same window so every number in a row describes one stretch of playback.
    """
    return rows[len(rows) // 4:] if len(rows) >= 8 else rows


def operating_points(fixtures: pathlib.Path):
    fixture = fixture_map()
    out = {}
    for rung in sorted({int(r) for r in fixture}, reverse=False):
        best = None
        for d in sorted(glob.glob(str(ROOT / "docs/measurements/*-logs")), reverse=True):
            p = pathlib.Path(d) / f"pipe_abr_pin_{rung}.log"
            if not p.exists():
                continue
            rows = settled(pin_samples(p, rung))
            if len(rows) < 8:
                continue
            # Newest capture wins: `sorted(reverse=True)` puts p2 ahead of p1b ahead of p1, which is
            # what makes the FIXTURE REBUILD visible instead of averaged away. A table that mixed
            # both packs would be wrong at every rung rather than at two of them.
            best = (pathlib.Path(d).name, rows)
            break
        if best is None:
            continue
        source, rows = best
        durs = {r["dur"] for r in rows}
        ts = round(statistics.median(r["media"] for r in rows))
        audio = audio_es_kbps(fixtures / fixture[str(rung)])
        out[rung] = {
            "source": source, "n": len(rows), "dur_ms": sorted(durs),
            "ts_kbps": ts,
            "ts_spread": (min(r["media"] for r in rows), max(r["media"] for r in rows)),
            "distinct_sizes": len({r["media"] for r in rows}),
            "audio_es_kbps": audio,
            "video_es_kbps": round((ts - audio) / TS_OVERHEAD) if audio else None,
            "overhead_ms": round(statistics.median(r["overhead"] for r in rows)),
            "buf_median_ms": round(statistics.median(r["buf"] for r in rows)),
            "fixture": fixture[str(rung)],
        }
    return out


def transaction_legs():
    legs = collections.defaultdict(list)
    for p in sorted(glob.glob(str(ROOT / "docs/measurements/*-logs/*.log"))):
        for m in RE_TX.finditer(pathlib.Path(p).read_text(errors="replace")):
            direction, outcome = m.group(1), m.group(4)
            def num(g):
                return None if m.group(g) == "none" else int(m.group(g))
            legs[(direction, outcome == "committed")].append(
                {"control": num(7), "warmup": num(11), "graded": num(12), "outcome": outcome})
    return legs


def leg_summary(rows):
    def med(key):
        vals = [r[key] for r in rows if r[key] is not None]
        return round(statistics.median(vals)) if vals else 0
    return {"n": len(rows), "control_plane_ms": med("control"),
            "warmup_acq_ms": med("warmup"), "graded_acq_ms": med("graded"),
            "outcomes": dict(collections.Counter(r["outcome"] for r in rows))}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fixtures", type=pathlib.Path, default=DEFAULT_FIXTURES)
    ap.add_argument("--rust", action="store_true", help="emit the Rust match arms")
    ap.add_argument("--rust-tx", action="store_true",
                    help="emit the Rust TransactionModel::measured body")
    args = ap.parse_args(argv)

    points = operating_points(args.fixtures)
    legs = {k: leg_summary(v) for k, v in transaction_legs().items()}

    if args.rust:
        print("        let (ts, audio, overhead) = match rung_request_kbps {")
        first = True
        for rung, p in sorted(points.items()):
            if p["audio_es_kbps"] is None:
                continue
            # Type suffixes ride the FIRST arm only, which is how the tuple's types are fixed and
            # how the existing file is written.
            sfx = ("u32", "u32", "i64") if first else ("", "", "")
            first = False
            print(f"            {rung:_} => ({p['ts_kbps']:_}{sfx[0]}, {p['audio_es_kbps']}{sfx[1]}, "
                  f"{p['overhead_ms']}{sfx[2]}),")
        print("            _ => return None,")
        print("        };")
        return 0

    if args.rust_tx:
        order = [("up_commit", ("Up", True)), ("up_reject", ("Up", False)),
                 ("down_commit", ("Down", True)), ("down_reject", ("Down", False))]
        print("        Self {")
        for field, key in order:
            if key not in legs:
                print(f"            {field}: None,")
                continue
            s = legs[key]
            print(f"            {field}: Some(TransactionCost {{ "
                  f"control_plane_ms: {s['control_plane_ms']}, "
                  f"warmup_acq_ms: {s['warmup_acq_ms']}, "
                  f"graded_acq_ms: {s['graded_acq_ms']} }}),")
        print("        }")
        return 0

    print(f"{'rung':>6} {'ts kbps':>8} {'spread':>15} {'sizes':>6} {'audio':>6} {'vid ES':>7} "
          f"{'ovh ms':>7} {'buf med':>8}   provenance")
    for rung, p in sorted(points.items()):
        lo, hi = p["ts_spread"]
        audio = p["audio_es_kbps"]
        print(f"{rung:>6} {p['ts_kbps']:>8} {f'{lo}-{hi}':>15} {p['distinct_sizes']:>6} "
              f"{(audio if audio is not None else '—'):>6} "
              f"{(p['video_es_kbps'] if p['video_es_kbps'] is not None else '—'):>7} "
              f"{p['overhead_ms']:>7} {p['buf_median_ms']:>8}   "
              f"{p['source']} n={p['n']} dur={p['dur_ms']} {p['fixture']}")

    print()
    print(f"{'leg':<16}{'n':>4}{'control':>9}{'warmup':>8}{'graded':>8}   outcomes")
    for direction in ("Up", "Down"):
        for commit in (True, False):
            key = (direction, commit)
            name = f"{direction}/{'commit' if commit else 'reject'}"
            if key not in legs:
                print(f"{name:<16}{0:>4}{'—':>9}{'—':>8}{'—':>8}   NEVER OBSERVED")
                continue
            s = legs[key]
            print(f"{name:<16}{s['n']:>4}{s['control_plane_ms']:>9}{s['warmup_acq_ms']:>8}"
                  f"{s['graded_acq_ms']:>8}   {s['outcomes']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
