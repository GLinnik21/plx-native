#!/usr/bin/env python3
"""M3 — measure what a rung actually COSTS the server, per actuator.

`HlsActuatorCatalog::measured()` carries a `production_load_pm` for each of its thirteen points,
and eleven of the thirteen are not measurements. `ladder.rs` says so at the table: two points are
empirical and the rest are "an ordering assumption". That table is one half of the two-constraint
admission rule — the half that refuses 4K on a fast link in front of a loaded PMS — so eleven
unmeasured numbers decide a real behaviour, and `docs/adaptive-playback-plan.md` blocks increment
I9 on this measurement for exactly that reason.

**What is measured.** For each request ceiling, one short-lived transcode session is registered and
N consecutive segments are fetched, giving

    rho = total_fetch_ms / media_duration_ms

per segment — the same quantity `SegmentSample::production_ratio_pm` computes at runtime, in the
same units, so the census and the controller cannot mean different things by it. rho is reported
COLD (the first segment, which the encoder has not produced ahead of) and WARM (the median of the
rest), because those are different questions and the catalog answers only the second.

**Why the comparison is on the RESIDUAL.** `production_load_pm` is a RELATIVE work figure
normalised so `P1080High` reads 1000, not an absolute ratio. Every fetch also carries a
per-segment overhead that has nothing to do with the rung — connection, request, container
muxing — so comparing raw rho against the table compares two different quantities. The residual

    load_j = 1000 * (rho_j - rho_floor) / (rho_top - rho_floor)

removes it, taking `rho_floor` from the lowest rung actually measured and `rho_top` from
`P1080High`, which is the point the table is normalised on. This mirrors `predicted_ratio_pm`'s
own construction rather than inventing a comparison.

**Both pacing legs are run and they are not interchangeable.** Back-to-back requests measure how
fast the encoder CAN produce; `--pace 2.0` holds one request per two seconds of media, which is
what a player actually does and therefore what the runtime ratio is compared against. The paced
leg is the one the catalog is meant to match; the back-to-back leg is reported beside it because
their difference IS the just-in-time term.

**Falsification, stated before the run** (`docs/adaptive-playback-plan.md` M3): residual loads
within +/-15% of the table uphold the "inert argmax" finding and keep the deferred quality scoring
closed. Any mid-ladder load off by more than 25% means the argmax is a re-parameterisation on
fresh numbers and has to be argued on those. This tool prints the verdict rather than leaving it
to be eyeballed.

**Privacy.** Nothing here talks to the PMS itself. Every request goes through
`tools/pms-hls-probe.py`, which registers and stops its own session and refuses to write an
artifact containing a token, a server address, a session id, a rating key or a title. This tool
reads only that tool's already-redacted `report.json`, and its own output carries rung numbers and
timings — never the item it measured, only the overlay KEY the operator passed, which is a shape
name (`movie_h264_ac3_1080p`) and not an identifier on anybody's server.

Runs on the dev Mac against the configured PMS. No television, no lock, no `make`.
"""

from __future__ import annotations

import argparse
import collections
import json
import re
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
LADDER_RS = REPO / "rust-modules" / "src" / "abr" / "ladder.rs"
PROBE = REPO / "tools" / "pms-hls-probe.py"


def read_ladder():
    """Parse the catalog out of `ladder.rs` rather than transcribing it.

    A transcribed table is a second copy of the thing under test, free to drift from it silently —
    and drifting is precisely what this census exists to detect, so a stale copy here would report
    agreement with a table nobody is running. Three tables are read: the `point(...)` list for the
    wire rate and the load, `kbps()` for the request ceiling, and `raster()` for the frame.
    """
    text = LADDER_RS.read_text()

    points = re.findall(
        r"point\(Rung::(\w+),\s*([\d_]+),\s*([\d_]+)\)", text
    )
    if not points:
        sys.exit("ladder.rs: no `point(...)` entries found — has the catalog moved?")

    def table(fn_name):
        # Each accessor is a `match self { Rung::X => V, ... }`; take the arms in order.
        start = text.index(f"fn {fn_name}(")
        body = text[start : text.index("\n    }", start)]
        return body

    kbps_body = table("kbps")
    kbps = {}
    for name, value in re.findall(r"Rung::(\w+) => ([\d_]+)", kbps_body):
        kbps.setdefault(name, int(value.replace("_", "")))

    raster_body = table("raster")
    raster = {}
    for arm in re.findall(r"((?:\s*\|?\s*Rung::\w+)+)\s*=>\s*\((\d+),\s*(\d+)\)", raster_body):
        names, w, h = arm
        for name in re.findall(r"Rung::(\w+)", names):
            raster.setdefault(name, (int(w), int(h)))

    rungs = []
    for name, wire, load in points:
        if name not in kbps or name not in raster:
            # `Uhd` has no `kbps()` arm in some shapes; skip rather than guess a ceiling.
            continue
        rungs.append(
            {
                "rung": name,
                "request_kbps": kbps[name],
                "expected_wire_kbps": int(wire.replace("_", "")),
                "production_load_pm": int(load.replace("_", "")),
                "raster": raster[name],
            }
        )
    return rungs


def probe_rung(item, rung, segments, pace, owner, workdir):
    """One ceiling, one session, N consecutive segments. Returns the probe's own report."""
    out = workdir / f"{rung['rung']}-{'paced' if pace else 'b2b'}"
    out.mkdir(parents=True, exist_ok=True)
    width, height = rung["raster"]
    cmd = [
        sys.executable,
        str(PROBE),
        "--item", item,
        "--bitrate", str(rung["request_kbps"]),
        "--resolution", f"{width}x{height}",
        "--fixed-segments", str(segments),
        "--out", str(out),
    ]
    if pace:
        cmd += ["--pace", str(pace)]
    if owner:
        cmd.append("--owner")
    done = subprocess.run(cmd, capture_output=True, text=True, timeout=900)
    report = out / "report.json"
    if not report.exists():
        return None, (done.stderr or done.stdout or "").strip()[-400:]
    return json.loads(report.read_text()), None


def rhos(report):
    """rho per sampled segment: fetch wall time over the media time it carries.

    A segment whose duration ffprobe could not read contributes NOTHING rather than a guess: the
    denominator is the whole measurement, and a defaulted 2 000 ms would silently turn an unknown
    into a confident number.
    """
    out = []
    for sample in report.get("segments") or []:
        timing = sample.get("timing") or {}
        total_ms = timing.get("total_ms")
        probe = sample.get("probe") or {}
        duration_s = ((probe.get("format") or {}).get("duration"))
        try:
            media_ms = float(duration_s) * 1000.0
        except (TypeError, ValueError):
            continue
        if not total_ms or media_ms <= 0:
            continue
        out.append(round(1000.0 * float(total_ms) / media_ms))  # per mille, as the runtime does
    return out


def output_shape(report):
    """What PMS ACTUALLY produced at this ceiling: median output raster and wire rate.

    **This is the discriminator the first census lacked, and without it the headline finding is
    ambiguous.** M3 read a 1080p source's ordering as INVERTING — the low rungs costing more wall
    clock than the high ones — and concluded the table is indexed by the wrong variable. There is a
    second explanation that the same numbers also fit and that nothing recorded could separate:
    **at a ceiling above what the source needs, PMS may stop transcoding and copy the video**, so
    the cheapest column would not be a cheap transcode at all but a REMUX, and a remux does not
    belong on a curve of encoder cost.

    The 4K row is what makes it worth checking rather than assuming. Against a 1080p source the
    `Uhd` request measured 58 pm where `P1080High` measured 105 — the same output raster (PMS never
    upscales), the same source, half the work. Two requests that differ only in a bitrate ceiling
    neither of which binds should not differ by 2x, and "one of them stopped re-encoding" is the
    obvious reason they might.

    So: `codec`, `width x height` and the delivered rate per rung. If the cheap rows come back at
    the SOURCE's own rate they are copies and the inversion is an artefact of mixing two operations;
    if they come back re-encoded at their own rate, the inversion is real and the table's variable
    is wrong, which is what I9 has to decide on.
    """
    rasters, rates, codecs = [], [], []
    for sample in report.get("segments") or []:
        probe = sample.get("probe") or {}
        fmt = probe.get("format") or {}
        try:
            media_s = float(fmt.get("duration"))
        except (TypeError, ValueError):
            continue
        for stream in probe.get("streams") or []:
            if stream.get("codec_type") != "video":
                continue
            if stream.get("width") and stream.get("height"):
                rasters.append(f"{stream['width']}x{stream['height']}")
            if stream.get("codec_name"):
                codecs.append(stream["codec_name"])
            break
        size = sample.get("bytes") or (sample.get("timing") or {}).get("bytes")
        if size and media_s > 0:
            rates.append(round(float(size) * 8.0 / media_s / 1000.0))
    def mode(xs):
        return collections.Counter(xs).most_common(1)[0][0] if xs else None
    return {
        "raster": mode(rasters),
        "codec": mode(codecs),
        "delivered_kbps": statistics.median(rates) if rates else None,
        "n": len(rates),
    }


def summarise(samples):
    if not samples:
        return {"cold_pm": None, "warm_pm": None, "n": 0}
    warm = samples[1:] or samples
    return {
        "cold_pm": samples[0],
        "warm_pm": round(statistics.median(warm)),
        "n": len(samples),
        "all_pm": samples,
    }


def residuals(rows, key, field="warm_pm"):
    """`load_j = 1000 * (rho_j - rho_floor) / (rho_top - rho_floor)`, the table's own normalisation.

    **Computed for COLD as well as warm, because the signal is not in the same place as the
    table assumes.** A warm segment is one the encoder produced before it was asked for, so its
    fetch measures TRANSFER; the production term only shows on a segment the encoder has not run
    ahead of, which on an idle PMS is the first one. Reporting a residual over a flat warm profile
    divides noise by noise, which is how a 2 pm spread became a "2187% deviation".

    A zero or negative span is recorded as such rather than as `None` per rung: it means every
    rung cost the same, which is an ANSWER about the ladder — not a missing measurement.
    """
    have = [r for r in rows if r[key].get(field) is not None]
    key_out = "residual_load_pm" if field == "warm_pm" else "residual_load_pm_cold"
    if len(have) < 2:
        return
    floor = min(r[key][field] for r in have)
    top = next(
        (r[key][field] for r in have if r["rung"] == "P1080High"),
        max(r[key][field] for r in have),
    )
    span = top - floor
    for r in rows:
        value = r[key].get(field)
        r[key][key_out] = (
            None if value is None or span <= 0 else round(1000 * (value - floor) / span)
        )
    if span <= 0:
        for r in rows:
            r[key][key_out + "_note"] = "span<=0: every rung cost the same on this axis"


def verdict(rows, key):
    """M3's falsification rule, applied rather than described — over the INTERIOR of the ladder.

    The two normalisation anchors are excluded because their agreement is definitional, not
    evidence: the rung supplying `rho_floor` is 0 by construction and `P1080High` is 1000 by
    construction. Including them makes the floor rung read as a 100% deviation from whatever the
    table happens to say about it, which is the loudest number in the output and means nothing.
    M3's rule is about MID-ladder loads, and this is what that phrase has to mean.
    """
    have = [r for r in rows if r[key].get("residual_load_pm") is not None]
    if not have:
        return "INCONCLUSIVE — no interior rung produced a comparable residual"

    # **A residual is only readable if the ladder's span exceeds the measurement's own noise.**
    # Not a chosen threshold: it is the same comparison `BufferEstimate::draining` makes against
    # `DRAIN_EPS_MS_PER_S` and `ui::idle` makes against a visibility floor — judge the signal
    # against the dispersion of the instrument, not against zero. Against a 1080p SOURCE every
    # rung's warm rho lands within a couple of per mille of every other, because the encoder is
    # not downscaling and the fetch is measuring transport; dividing a 2 pm span by itself
    # produced a "2187% deviation" that says nothing about the table at all.
    spreads = []
    for r in rows:
        samples = r[key].get("all_pm") or []
        warm = samples[1:] or samples
        if len(warm) >= 2:
            spreads.append(max(warm) - min(warm))
    noise = statistics.median(spreads) if spreads else 0
    raw = [r[key]["warm_pm"] for r in have if r[key].get("warm_pm") is not None]
    span_pm = (max(raw) - min(raw)) if raw else 0
    if span_pm <= noise:
        return (
            f"INCONCLUSIVE — the ladder spans {span_pm}pm of rho against {noise}pm of within-rung "
            f"spread, so no residual is resolvable. This is the expected reading against a source "
            f"the encoder does not have to downscale; use a 4K source to separate the rungs"
        )
    # **Is the measured ordering even the table's ordering?** `production_load_pm` is monotone in
    # the rung, and a percentage deviation silently assumes that shape is right. Count concordant
    # against discordant pairs first: a majority-discordant ladder is not a mis-CALIBRATED table,
    # it is a table of the wrong VARIABLE, and reporting it as a percentage would hide that.
    pairs = [(r["production_load_pm"], r[key]["warm_pm"]) for r in have]
    con = dis = 0
    for i in range(len(pairs)):
        for j in range(i + 1, len(pairs)):
            (t1, m1), (t2, m2) = pairs[i], pairs[j]
            if t1 == t2 or m1 == m2:
                continue
            if (t1 < t2) == (m1 < m2):
                con += 1
            else:
                dis += 1
    if dis > con:
        return (
            f"INVERTED — {dis} of {con + dis} rung pairs cost the OPPOSITE of what the table "
            f"orders them. Production cost is not a function of the target rung alone: against a "
            f"source at raster R, a target BELOW R must be downscaled while a target AT R is a "
            f"near-copy, so the cheap end of the ladder is the expensive end to produce. "
            f"`production_load_pm` is indexed by target only and cannot express that"
        )
    floor_value = min(r[key]["residual_load_pm"] for r in have)
    # EVERY rung tied at the floor is an anchor, not just one of them. Two rungs measuring the
    # same cost is a real finding — the table separates P240 and P480 by 2x and the server does
    # not — but it is a finding about the TABLE'S ORDERING, not a percentage deviation, and
    # whichever tied rung is not chosen as the reference would otherwise read as 100% off.
    anchors = {"P1080High"} | {
        r["rung"] for r in have if r[key]["residual_load_pm"] == floor_value
    }
    tied = sorted(anchors - {"P1080High"})
    worst = None
    for r in rows:
        got = r[key].get("residual_load_pm")
        want = r["production_load_pm"]
        if got is None or want <= 0 or r["rung"] in anchors:
            continue
        dev = abs(got - want) / want
        r[key]["deviation_pct"] = round(dev * 100, 1)
        if worst is None or dev > worst[1]:
            worst = (r["rung"], dev)
    if worst is None:
        return "INCONCLUSIVE — no interior rung produced a comparable residual"
    name, dev = worst
    floor_note = (
        f" [floor-tied and excluded: {', '.join(tied)}]" if len(tied) > 1 else ""
    )
    if dev <= 0.15:
        return f"UPHELD — every interior residual within 15% (worst {name} at {dev*100:.1f}%){floor_note}"
    if dev <= 0.25:
        return f"MARGINAL — worst {name} at {dev*100:.1f}%, between the 15% and 25% rules{floor_note}"
    return (
        f"REFUTED — {name} is off by {dev*100:.1f}%; the deferred argmax is a re-parameterisation "
        f"on fresh numbers and must be argued on those{floor_note}"
    )


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--item", required=True, help="overlay item key (a SHAPE name, never a rating key)")
    ap.add_argument("--segments", type=int, default=12, help="consecutive segments per rung (M3 asks for >=10)")
    ap.add_argument("--pace", type=float, default=2.0, help="paced-leg seconds per request; 0 disables that leg")
    ap.add_argument("--no-b2b", action="store_true", help="skip the back-to-back leg")
    ap.add_argument("--owner", action="store_true", help="use the owner token")
    ap.add_argument("--only", help="comma-separated rung names, for a partial re-run")
    ap.add_argument("--out", help="where to write census.json (default: a private temp dir)")
    args = ap.parse_args()

    rungs = read_ladder()
    if args.only:
        want = {n.strip() for n in args.only.split(",")}
        rungs = [r for r in rungs if r["rung"] in want]
    if not rungs:
        sys.exit("no rungs selected")

    workdir = Path(args.out) if args.out else Path(tempfile.mkdtemp(prefix="abr-census-"))
    workdir.mkdir(parents=True, exist_ok=True)

    legs = []
    if not args.no_b2b:
        legs.append(("back_to_back", None))
    if args.pace:
        legs.append(("paced", args.pace))

    rows = []
    for rung in rungs:
        row = dict(rung)
        for key, pace in legs:
            print(f"  {rung['rung']:12s} {rung['request_kbps']:6d} kbps  {key:13s} ...", flush=True)
            report, err = probe_rung(args.item, rung, args.segments, pace, args.owner, workdir)
            if report is None:
                row[key] = {"cold_pm": None, "warm_pm": None, "n": 0, "error": err}
                print(f"      probe failed: {err}", flush=True)
                continue
            row[key] = summarise(rhos(report))
            row[key]["output"] = output_shape(report)
            out_s = row[key]["output"]
            print(
                f"      cold={row[key]['cold_pm']}pm warm={row[key]['warm_pm']}pm "
                f"n={row[key]['n']} out={out_s['codec']} {out_s['raster']} "
                f"{out_s['delivered_kbps']}kbps",
                flush=True,
            )
        rows.append(row)

    out = {"item_key": args.item, "segments_per_rung": args.segments, "rungs": rows, "verdict": {}}
    for key, _ in legs:
        residuals(rows, key, "warm_pm")
        residuals(rows, key, "cold_pm")
        out["verdict"][key] = verdict(rows, key)

    census = workdir / "census.json"
    census.write_text(json.dumps(out, indent=2, sort_keys=True) + "\n")

    # The output column is printed beside the cost, because reading a cost curve without knowing
    # which rows were re-encoded is the ambiguity `output_shape` exists to remove.
    print(f"\n{'rung':12s} {'req':>6s} {'table':>6s} " + " ".join(f"{k[:9]:>9s} {'resid':>6s} {'dev%':>6s}" for k, _ in legs) + f" {'output':>22s}")
    for r in rows:
        line = f"{r['rung']:12s} {r['request_kbps']:6d} {r['production_load_pm']:6d} "
        for key, _ in legs:
            d = r.get(key, {})
            line += f" {str(d.get('warm_pm')):>9s} {str(d.get('residual_load_pm')):>6s} {str(d.get('deviation_pct')):>6s}"
        last = (r.get(legs[-1][0]) or {}).get("output") or {}
        line += f" {str(last.get('raster')):>10s} {str(last.get('delivered_kbps')):>7s}kbps"
        print(line)
    print()
    for key, _ in legs:
        print(f"  {key}: {out['verdict'][key]}")
    print(f"\ncensus: {census}")


if __name__ == "__main__":
    main()
