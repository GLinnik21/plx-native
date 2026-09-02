#!/usr/bin/env python3
"""Independently replay the finite-episode conservation arithmetic in `abr: window`.

For every repeatable completed acquisition admitted to the live episode, the controller records
acquisition time ``A_i`` and playable media duration ``D_i``. This grader recomputes the quantities
that decide the current HLS point::

    sustainable  <=>  sum A_i <= sum D_i
    excess       =    sum max(A_i - D_i, 0)
    runway       =    excess + max min(A_i, D_i)
    survivable   <=>  playable_reserve >= runway

`prod` is an integer per-mille and therefore identifies an interval, not one acquisition time. The
grader propagates that exact quantisation interval through every monotone expression; it has no
tolerance. Candidate commits carry the one `(A,D)` sample that seeds the reset episode, while rejected
or censored candidates contribute nothing. An abandoned current response is likewise excluded by
the logged `complete=0` bit.

Usage:
    tools/abr-window-grade.py <current-exact-controller-capture>.log

Historical order-statistic traces are deliberately rejected as incompatible rather than graded
under rules that did not produce them.
"""

from __future__ import annotations

import glob
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "tests"))
from run import (  # noqa: E402
    RE_ABR_COMMIT as RE_COMMIT,
    RE_ABR_SAMPLE as RE_SAMPLE,
    RE_ABR_SEED as RE_SEED,
    RE_ABR_TX as RE_TX,
    RE_ABR_WINDOW as RE_WINDOW,
    TX_FIELDS,
    WINDOW_FIELDS,
)

# Every wire pattern and field list is imported from `tests/run.py`, the harness contract owner.
# Keeping no local regex copy matters: a telemetry field change must either keep this replay
# parseable or fail the shared contract tests, never silently turn a real trace into zero matches.

def parsed_fields(fields, groups):
    """Give one harness-owned regex its harness-owned field names and numeric types."""
    out = {}
    for name, raw in zip(fields, groups):
        if raw == "none":
            out[name] = None
        elif raw is not None and (raw.isdigit() or (raw.startswith("-") and raw[1:].isdigit())):
            out[name] = int(raw)
        else:
            out[name] = raw
    return out


def paired(lines):
    """The controller's finite-episode events in exact log order.

    Segments are paired by ADJACENCY because one call site writes `abr: sample` and `abr: window`
    in that order for one segment. A `window` line with no `sample` before it means the two drifted
    apart and every number below would be attributed to the wrong segment, so it is an error rather
    than a skip.

    Only a transaction corroborated by BOTH `abr: committed` and the following
    `abr: tx outcome=committed` becomes a `commit` event. The first line is emitted after the
    controller reset+seed and the second carries the seed `(A,D)`; neither line alone is enough.
    A rejected candidate may have completed a useful request, but the live controller deliberately
    preserves the old operating-point episode, so it contributes no event.
    """
    out, pending_sample, pending_commit = [], None, None
    for line in lines:
        m = RE_SEED.search(line)
        if m:
            if pending_sample is not None:
                raise SystemExit("`abr: seed` interrupted a sample/window pair")
            if pending_commit is not None:
                out.append(("error", "`abr: committed` was not followed by its transaction"))
                pending_commit = None
            out.append(("seed",))
            continue
        m = RE_COMMIT.search(line)
        if m:
            if pending_sample is not None:
                raise SystemExit("`abr: committed` interrupted a sample/window pair")
            if pending_commit is not None:
                out.append(("error", "two `abr: committed` lines precede one transaction"))
            pending_commit = (m.group(1), int(m.group(2)))
            continue
        m = RE_TX.search(line)
        if m:
            if pending_sample is not None:
                raise SystemExit("`abr: tx` interrupted a sample/window pair")
            tx = parsed_fields(TX_FIELDS, m.groups())
            if tx["outcome"] == "committed":
                marker = (tx["direction"], tx["to_kbps"])
                if pending_commit is None:
                    out.append(("error", "`outcome=committed` has no preceding commit certificate"))
                    continue
                if pending_commit != marker:
                    out.append((
                        "error",
                        f"commit marker {pending_commit} disagrees with transaction {marker}",
                    ))
                    pending_commit = None
                    continue
                acq_ms = tx["candidate_acq_ms"]
                byte_count = tx["candidate_bytes"]
                dur_ms = tx["candidate_dur_ms"]
                if acq_ms is None or acq_ms < 0 or byte_count <= 0 or dur_ms <= 0:
                    out.append(("error", "committed transaction has no complete candidate evidence"))
                    pending_commit = None
                    continue
                out.append(("commit", {
                    "acq_lo_us": max(1, acq_ms * 1_000),
                    "acq_hi_us": (acq_ms + 1) * 1_000,
                    "dur_us": dur_ms * 1_000,
                }))
            elif pending_commit is not None:
                out.append((
                    "error",
                    f"commit marker {pending_commit} is followed by outcome={tx['outcome']}",
                ))
            pending_commit = None
            continue
        m = RE_SAMPLE.search(line)
        if m:
            if pending_sample is not None:
                raise SystemExit("two `abr: sample` lines with no intervening `abr: window`")
            raw_buf = m.group(4)
            pending_sample = {
                "buf_ms": None if raw_buf == "none" else int(raw_buf.rstrip("ms")),
                "dur_ms": int(m.group(7)),
                "prod_pm": int(m.group(8)),
                # Absent only on archived traces from before the censored-acquisition path existed.
                "completed": None if m.group(12) is None else bool(int(m.group(12))),
            }
            continue
        m = RE_WINDOW.search(line)
        if not m:
            continue
        if pending_sample is None:
            raise SystemExit("`abr: window` with no preceding `abr: sample`; the pairing is broken")
        window = parsed_fields(WINDOW_FIELDS, m.groups())
        # Keep the short names used by the original report without letting positional capture
        # numbers define their meaning.
        window["sus"] = window.pop("sustainable")
        window["sur"] = window.pop("survivable")
        out.append(("segment", pending_sample, window))
        pending_sample = None
    if pending_commit is not None:
        out.append(("error", "`abr: committed` was not followed by its transaction"))
    if pending_sample is not None:
        raise SystemExit("`abr: sample` at end of trace has no following `abr: window`")
    return out


def acquisition_interval(sample) -> tuple[int, int | None]:
    """`[lo, hi)` microseconds admitted by a truncated `prod=` at this duration."""
    lo = max(1, sample["prod_pm"] * sample["dur_ms"])
    if sample["prod_pm"] == 2**32 - 1:
        return lo, None
    return lo, (sample["prod_pm"] + 1) * sample["dur_ms"]


def finite_terms(entries, high=False):
    """The four conservation terms at one endpoint of every acquisition interval."""
    acquisitions = [e["acq_hi_us"] - 1 if high else e["acq_lo_us"] for e in entries]
    durations = [e["dur_us"] for e in entries]
    demand = sum(acquisitions)
    supply = sum(durations)
    excess = sum(max(a - d, 0) for a, d in zip(acquisitions, durations))
    terminal = max((min(a, d) for a, d in zip(acquisitions, durations)), default=0)
    return {
        "demand": demand,
        "supply": supply,
        "excess": excess,
        "runway": excess + terminal,
    }


def grade(rows):
    """Replay attributable spans of the current operating-point episode.

    Commit resets carry a reconstructible seed. A delivery-collapse reset has only the reset
    counter, so it makes later rows ungraded until a marked seed or commit restores attribution.
    """
    if not rows:
        return None

    episode = []
    checked = disagree = filling = resets = candidates = ungraded = 0
    ambiguous_sus = ambiguous_sur = saturated = epochs = 0
    seen_resets = 0
    pending_commits = 0
    last_reserve_ms = 0
    state_known = True
    for row in rows:
        if row[0] == "error":
            print(f"  ! {row[1]}")
            disagree += 1
            # A broken commit certificate means the operating-point episode may have changed without
            # a reconstructible seed. Only a later `abr: seed` (or a fully certified commit) can
            # make subsequent arithmetic attributable again.
            state_known = False
            continue
        if row[0] == "seed":
            episode = []
            seen_resets = 0
            pending_commits = 0
            last_reserve_ms = 0
            state_known = True
            epochs += 1
            continue
        if row[0] == "commit":
            # `Controller::commit` retires the old operating point, then atomically seeds the new
            # episode with this completed candidate. A reject never reaches this event.
            episode = [row[1]]
            pending_commits += 1
            candidates += 1
            state_known = True
            continue
        _, sample, window = row
        expected_resets = seen_resets + pending_commits
        if window["resets"] != expected_resets:
            direction = "BACKWARDS" if window["resets"] < seen_resets else "has no commit evidence"
            print(f"  ! reset={window['resets']} {direction}; expected exactly {expected_resets}")
            disagree += 1
            state_known = False
        else:
            resets += pending_commits
        seen_resets = window["resets"]
        pending_commits = 0

        if sample["buf_ms"] is not None:
            last_reserve_ms = sample["buf_ms"]
        if sample["dur_ms"] != window["dur_ms"]:
            print(
                f"  ! sample dur={sample['dur_ms']}ms but window dur={window['dur_ms']}ms"
            )
            disagree += 1

        # These are generation invariants, not a mode switch controlled by the values under test.
        # A corrupt `eps=100` must be reported while the finite-episode arithmetic below remains live.
        for name, got, expected in (
            ("eps", window["eps_pm"], 0),
            ("clamp", window["clamp"], 0),
            ("bound", window["bound_ms"], -1),
            ("want", window["want"], window["have"]),
        ):
            if got != expected:
                print(f"  ! exact finite episode reports {name}={got}, expected {expected}")
                disagree += 1

        completed = sample["completed"]
        if completed is None:
            print("  ! exact window has no complete= bit; censored input cannot be replayed")
            disagree += 1
            state_known = False
        elif completed and state_known:
            lo, hi = acquisition_interval(sample)
            episode.append({
                "acq_lo_us": lo,
                "acq_hi_us": hi,
                # D comes from the independently paired sample. The window's own `dur` is checked
                # above but never gets to define the expected supply it is being graded against.
                "dur_us": sample["dur_ms"] * 1_000,
            })

        if not state_known:
            ungraded += 1
            continue

        have = len(episode)
        if window["have"] != have:
            print(f"  ! have={window['have']} but the certified event stream gives {have}")
            disagree += 1

        if have == 0:
            expected_empty = {
                "verdict": "filling", "demand_ms": -1, "supply_ms": -1,
                "excess_ms": -1, "runway_ms": -1, "sus": 0, "sur": 0,
            }
            for name, expected in expected_empty.items():
                if window[name] != expected:
                    print(f"  ! empty episode reports {name}={window[name]}, expected {expected}")
                    disagree += 1
            filling += 1
            continue

        expected_verdict = "admit" if window["sus"] == 1 and window["sur"] == 1 else "refuse"
        if window["sus"] not in (0, 1) or window["sur"] not in (0, 1):
            print(f"  ! non-boolean sus/sur={window['sus']}/{window['sur']}")
            disagree += 1
        if window["verdict"] != expected_verdict:
            print(
                f"  ! verdict={window['verdict']} but sus={window['sus']} sur={window['sur']} "
                f"requires {expected_verdict}"
            )
            disagree += 1

        low = finite_terms(episode, high=False)
        supply_ms = low["supply"] // 1_000
        if window["supply_ms"] != supply_ms:
            print(f"  ! supply={window['supply_ms']}ms, paired sample durations sum to {supply_ms}ms")
            disagree += 1

        if any(entry["acq_hi_us"] is None for entry in episode):
            # `prod=u32::MAX` is a saturated lower bound, not a finite quantisation bin. Preserve
            # membership/have and exact supply/verdict coherence, but never call the remaining
            # terms fully graded without an upper endpoint.
            print("  ? saturated prod interval: demand/excess/runway and sus/sur are lower-bound only")
            saturated += 1
            ungraded += 1
            continue

        high = finite_terms(episode, high=True)

        checked += 1
        for name, got in (
            ("demand", window["demand_ms"]),
            ("excess", window["excess_ms"]),
            ("runway", window["runway_ms"]),
        ):
            want_lo = low[name] // 1_000
            want_hi = high[name] // 1_000
            if not want_lo <= got <= want_hi:
                print(f"  ! {name}={got}ms outside the logged interval [{want_lo},{want_hi}]ms")
                disagree += 1

        always_sustainable = high["demand"] <= low["supply"]
        never_sustainable = low["demand"] > low["supply"]
        if always_sustainable and not window["sus"]:
            print("  ! sus=0 but the whole interval is sustainable")
            disagree += 1
        if never_sustainable and window["sus"]:
            print("  ! sus=1 but the whole interval is unsustainable")
            disagree += 1
        if not always_sustainable and not never_sustainable:
            ambiguous_sus += 1
        reserve_us = last_reserve_ms * 1_000
        always_survivable = reserve_us >= high["runway"]
        never_survivable = reserve_us < low["runway"]
        if always_survivable and not window["sur"]:
            print("  ! sur=0 but reserve covers the whole runway interval")
            disagree += 1
        if never_survivable and window["sur"]:
            print("  ! sur=1 but reserve is below the whole runway interval")
            disagree += 1
        if not always_survivable and not never_survivable:
            ambiguous_sur += 1
    return {"rows": len(rows), "checked": checked, "filling": filling,
            "disagree": disagree, "resets": resets, "candidates": candidates,
            "ambiguous_sus": ambiguous_sus, "ambiguous_sur": ambiguous_sur,
            "ungraded": ungraded, "saturated": saturated, "epochs": epochs}


def occupancy(rows):
    """What the live finite-episode gate saw, as a distribution -- what pass/fail cannot report.

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
    print(f"{'case':<34} {'lines':>6} {'graded':>7} {'ungr':>5} {'amb':>7} {'cand':>5} "
          f"{'reset':>6} {'disagree':>9}")
    total_checked = total_disagree = total_ungraded = total_ambiguous = empty_files = 0
    occ = []
    for path in paths:
        rows = paired(pathlib.Path(path).read_text(errors="replace").splitlines())
        r = grade(rows)
        if r is None:
            empty_files += 1
            print(f"{pathlib.Path(path).stem[:34]:<34} {'0':>6} {'0':>7} {'0':>5} "
                  f"{'0':>7} {'0':>5} {'0':>6} {'NO TRACE':>9}")
            continue
        name = pathlib.Path(path).stem[:34]
        ambiguous = r["ambiguous_sus"] + r["ambiguous_sur"]
        print(f"{name:<34} {r['rows']:>6} {r['checked']:>7} {r['ungraded']:>5} "
              f"{ambiguous:>7} {r['candidates']:>5} {r['resets']:>6} {r['disagree']:>9}")
        total_checked += r["checked"]
        total_disagree += r["disagree"]
        total_ungraded += r["ungraded"]
        total_ambiguous += ambiguous
        o = occupancy(rows)
        if o:
            occ.append((name, o))

    print()
    print(f"{total_checked} fully graded lines, {total_ungraded} ungraded, "
          f"{total_ambiguous} threshold-ambiguous flag checks, "
          f"{total_disagree} disagreements, {empty_files} file(s) with no current trace")
    if occ:
        print()
        print("what the shadow actually saw (a confirmed arithmetic on an idle link proves little):")
        print(f"  {'case':<34} {'admit':>6} {'refuse':>7} {'load min':>9} {'mean':>7} "
              f"{'max':>7} {'exc>0':>6} {'exc max':>8}")
        for name, o in occ:
            print(f"  {name:<34} {o['verdicts']['admit']:>6} {o['verdicts']['refuse']:>7} "
                  f"{o['load_min']:>9.2f} {o['load_mean']:>7.2f} {o['load_max']:>7.2f} "
                  f"{o['excess_nonzero']:>6} {o['excess_max']:>7}ms")
    return 1 if total_disagree or total_ungraded or empty_files else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
