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

1. **Operating points** — `ts_kbps`, `audio_es_kbps`, `overhead_ms` plus the master declaration
   and decoded raster per rung, from the M4 pin
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
import importlib.util
import json
import os
import pathlib
import re
import statistics
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_FIXTURES = pathlib.Path(os.path.expanduser("~/plxnative-fixtures/pipeline"))

sys.path.insert(0, str(ROOT / "tests"))
from run import RE_ABR_SAMPLE as RE_SAMPLE  # noqa: E402

# **`abr: sample` is `tests/run.py`'s pattern, not a copy of it.** This file scrapes `ABR_FIXTURE`
# out of `serve_fixtures.py` at run time for exactly this reason, thirteen lines below, and the
# argument is the same one level up: a copied regex sits outside `tests/test_harness.py`'s contract
# test, so a field added to the Rust format string reddens that test and leaves this tool matching
# nothing -- and a table built from zero samples is a table this tool prints without complaint.
# Measured before the swap over the 81 logs under `docs/measurements/`: 4627 samples either way.
#
# **`abr: tx` is deliberately NOT imported, and the same measurement is why.** `run.RE_ABR_TX`
# names the CURRENT field set exactly and matched 382 of the 504 transaction lines in that corpus;
# the prefix below, which stops at `graded=`, matched 479. The corpus is append-only and spans
# several instrumentation generations, so importing here would silently drop a fifth of the
# calibration evidence. Same trade as `tools/abr-window-grade.py`'s `RE_TX_GRADED`.
RE_TX = re.compile(
    r"abr: tx (\w+) (\d+)->(\d+)kbps outcome=(\S+) decided=(-?\d+|none)ms total=(-?\d+)ms "
    r"control=(-?\d+|none)ms prime=(-?\d+|none)ms master=(-?\d+|none)ms media=(-?\d+|none)ms "
    r"warmup=(-?\d+|none)ms graded=(-?\d+|none)ms"
)
RE_MASTER = re.compile(r"hls: master one-variant bandwidth=(\d+)")
RE_RASTER = re.compile(r"hls: segment=\d+ bytes=\d+ raster=(\d+)x(\d+)")

# `sim.rs`'s own documented assumption: the AU queues hold demuxed ELEMENTARY bytes while `media=`
# is measured off the TS wire. 1.04 is an assumed transport-stream overhead -- an assumption, not a
# measurement, and `sim.rs` says so where it uses it.
#
# Read from `tools/abr-plant-sweep.py`, which owns the queue geometry; `tools/abr-tx-report.py`
# states the same direction of dependency and this had gone the other way, so one constant was
# written out twice with both copies claiming to mirror `sim.rs`.
def _ts_overhead() -> float:
    spec = importlib.util.spec_from_file_location(
        "plant_sweep", ROOT / "tools" / "abr-plant-sweep.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.TS_OVERHEAD


TS_OVERHEAD = _ts_overhead()

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


def capture_added_at(directory: pathlib.Path):
    """Unix time of the commit that FIRST ADDED this capture, or `None` if it is uncommitted.

    Uncommitted means a capture still in the working tree, which is the newest thing there can be.
    """
    out = subprocess.run(
        ["git", "log", "--diff-filter=A", "--format=%ct", "-1", "--", str(directory)],
        cwd=ROOT, capture_output=True, text=True)
    stamp = out.stdout.strip()
    return int(stamp) if out.returncode == 0 and stamp else None


def captures_newest_first():
    """Every capture directory that exists, newest first, **derived from git**.

    # Two wrong answers preceded this one, and the second was worse than the first

    It was `sorted(reverse=True)`, which is not chronological: the names mix PHASE numbering
    (`p1`, `p1b`, `p2`, `p2h`) with INCREMENT numbering (`i2`, `j3`, `j3a`, `j3b`), so every `p*`
    sorts above every `j*`. The moment a `j3b` capture landed, "newest wins" silently meant "p2
    wins" -- the stale-table failure this file exists to prevent, recurring in the mechanism meant
    to prevent it.

    It was then replaced by a HAND-WRITTEN chronology, on the stated grounds that "git cannot
    rescue it either: this branch's captures all carry the same commit date." **That was false, and
    the hand-written list was wrong in two places** -- `j3-decides-logs` is NEWER than
    `j3a-window-logs`, not older, and `p2-logs` is newer than `p2h-logs`. The claim came from
    reading `git log --format=%cs`, which is the DAY, and concluding the captures were
    indistinguishable. `--diff-filter=A --format=%ct` separates them to the second, and it is the
    right question anyway: a capture's chronology is when it was ADDED, not when its directory was
    last touched.

    So the order is derived, not stated, and the failure mode of the previous two versions -- a
    human placing a capture wrongly, or forgetting to place it at all -- cannot occur.
    """
    out = []
    for d in glob.glob(str(ROOT / "docs/measurements/*-logs")):
        path = pathlib.Path(d)
        added = capture_added_at(path)
        # An uncommitted capture sorts newest: it is a capture being taken right now.
        out.append((added if added is not None else float("inf"), path))
    return [pathlib.Path(path) for _, path in sorted(out, key=lambda pair: pair[0], reverse=True)]


def operating_points(fixtures: pathlib.Path):
    fixture = fixture_map()
    out = {}
    # Hoisted: chronology is a property of the capture directories, not of the rung. Called inside
    # the loop it forked one `git log` per capture per rung -- 195 subprocesses and ~3.4 s on this
    # tree, for 15 distinct answers.
    captures = captures_newest_first()
    for rung in sorted({int(r) for r in fixture}):
        best = None
        for d in captures:
            p = d / f"pipe_abr_pin_{rung}.log"
            if not p.exists():
                continue
            rows = settled(pin_samples(p, rung))
            if len(rows) < 8:
                continue
            # Newest capture wins, and the search stops at the FIRST one that has this rung —
            # a rung is never averaged across captures, which is what makes a fixture rebuild
            # visible instead of blended away.
            #
            # **It is per rung, not wholesale, and that is a real seam.** No single capture has
            # pinned every rung, so a table normally draws from two or three; and while a capture
            # is still being taken it draws from a PARTIAL one, mixing generations. The defence is
            # not a rule, it is the `provenance` column, which names the capture behind every row.
            # Read it before trusting a table: rows from different captures are legitimate only
            # while the fixture pack is unchanged between them.
            text = p.read_text(errors="replace")
            masters = [int(value) for value in RE_MASTER.findall(text)]
            rasters = [(int(w), int(h)) for w, h in RE_RASTER.findall(text)]
            if not masters or not rasters:
                continue
            best = (d.name, rows, masters[-1], rasters[-1])
            break
        if best is None:
            continue
        source, rows, declared_bps, raster = best
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
            "declared_kbps": declared_bps // 1_000,
            "decoded_width": raster[0],
            "decoded_height": raster[1],
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


#: Outcomes whose cost is a DEADLINE rather than an acquisition. A transaction that hit its
#: deadline never completed a fetch, so it logs `warmup=none` -- and a median over "none" is
#: nothing, which a constant-cost leg records as ZERO. Its real cost is `min(acceptance budget,
#: reserve)`, a state variable, so there is no constant to record at all.
DEADLINE_OUTCOMES = ("warmup_deadline", "graded_deadline")


def leg_summary(rows):
    def med(key):
        vals = [r[key] for r in rows if r[key] is not None]
        return round(statistics.median(vals)) if vals else 0
    deadlines = sum(1 for r in rows if r["outcome"] in DEADLINE_OUTCOMES)
    return {"n": len(rows), "control_plane_ms": med("control"),
            "warmup_acq_ms": med("warmup"), "graded_acq_ms": med("graded"),
            "deadline_aborts": deadlines,
            # **A leg every one of whose members hit a deadline has no constant cost**, and
            # emitting one is worse than emitting nothing: the plant would model a downshift
            # reject as FREE -- 5 ms of control plane for a transaction that really cost 2 226 ms,
            # a 445x understatement -- and could not represent the failure the deadline exists to
            # bound. The previous plant made the mirror-image error, charging one flat 4 600 ms to
            # all four legs, which made `T_down` on a collapsing link unrepresentable. `sim.rs`
            # refuses an absent leg loudly, which is the right failure to have.
            #
            # A MIXED leg is still understated and this does not fix that: `up_reject` medians the
            # two `graded_deadline` members that measured a warm-up and silently omits the two
            # `warmup_deadline` ones, whose cost was their deadline. `deadline_aborts` is reported
            # beside `n` so the ratio is visible rather than implied.
            # The test is whether ANY member contributed an acquisition measurement, not what the
            # outcomes were called: a `graded_deadline` transaction completed its warm-up and so
            # carries a real number, while a `warmup_deadline` one never completed a fetch at all.
            # An outcome-name test flagged `up_reject` (2 of its 4 are graded_deadline, with real
            # warm-ups) and would have thrown away a measured leg.
            "costless": bool(rows) and all(
                r["warmup"] is None and r["graded"] is None for r in rows),
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
        print("        let (ts, audio, overhead, declared, width, height) = match rung_request_kbps {")
        first = True
        for rung, p in sorted(points.items()):
            if p["audio_es_kbps"] is None:
                continue
            # Type suffixes ride the FIRST arm only, which is how the tuple's types are fixed and
            # how the existing file is written.
            sfx = ("u32", "u32", "i64", "u32", "u16", "u16") if first else ("", "", "", "", "", "")
            first = False
            print(f"            {rung:_} => ({p['ts_kbps']:_}{sfx[0]}, {p['audio_es_kbps']}{sfx[1]}, "
                  f"{p['overhead_ms']}{sfx[2]}, {p['declared_kbps']:_}{sfx[3]}, "
                  f"{p['decoded_width']}{sfx[4]}, {p['decoded_height']}{sfx[5]}),")
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
            if s["costless"]:
                # Observed, and STILL `None` — for a different reason than "never seen". Every
                # member hit a deadline, so its cost is `min(acceptance, reserve)` rather than an
                # acquisition, and a constant here would record it as free. See DEADLINE_OUTCOMES.
                print(f"            {field}: None,  // {s['n']} observed, all deadline aborts: "
                      "cost is the reserve, not a constant")
                continue
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
