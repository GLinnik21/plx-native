#!/usr/bin/env python3
"""Derive ci-summary.json (the page's "now" block) from ci-history.json and build-history.json.

usage: tools/ci-summary.py --history FILE [--bench FILE] --out FILE [--now ISO]

The page answers "what do I wait for today, is it getting better or worse, and what should be fixed
next". This is the arithmetic behind that, as pure functions (`summarize(history, bench, now)` takes
parsed documents and returns the document; nothing in the core reads a file or the clock), so the
page, a test and a later alerting job all get the same answer. The `ci-metrics` workflow runs it
after tools/ci-history.py and commits the result beside it.

The document is `{"schema": 2, "generated", "thresholds", "metrics", "slowest_ci", "slowest_local",
"regressions"}`:

  metrics      one entry per series with a sample in the last 14 days (a removed job is not "now"),
               IN THE ORDER A READER WANTS THEM (see "Order and selection"): id, label, group ("ci" or
               "local"), kind ("wait", "cost", "local", "job"), shown, unit ("s"), now, prev7, d30,
               delta7 / delta30 (now minus that window, seconds), change7_pct, change30_pct, state
               ("worse", "better", "flat", "new") against the week before, state30 (the same verdict
               against the window 30 days ago, "new" when there is none), threshold_pct, threshold_abs,
               n_now, n_prev; a local row that is a part of another one (check.cargo of check) has
               `parent`, and the whole has `parts` (largest first: the first is the long pole).
  slowest_ci   the five longest CI jobs by `now`. Waits, runner time and local rows are not jobs.
  slowest_local  the five longest local steps by `now`, a part never listed beside its whole, and the
               no-op build (a health check, not a wait) left out. CI jobs and the benchmark are two
               populations: the benchmark runs on a 3-core GitHub macOS runner, several times slower than
               a developer's Mac, so the two are never ranked against each other.
  regressions  every metric that is "worse", with the day it started and the commits since.

Series (ids). Waits are per COMMIT, not per run:
  pr_wait        a pull request's head commit: the slowest of its runs (CI, Simulator CI, the nightly's
                 dry run), pull_request runs only. A commit with a red run has no honest wait and is left out.
  push_wait      the same for a push to main (CI and Simulator CI). This is the page's old "wait".
  <wf>|<job>     one green job of CI ("ci") or Simulator CI ("sim"), PUSH runs only: a pull request's
                 run is partial more often (path filters, a superseded push) and would skew a job's line.
  nightly|<job>  one green job of the nightly's dry run on a pull request.
  minutes        runner seconds per push (every green job of CI and Simulator CI added up).
  bench.<row>    a row of the daily benchmark (build-history.json), its median.
A run that was re-run counts `wall` (created to last update: what the developer waited across the
attempts) instead of its longest job; the jobs of such a run only hold the last attempt.

Windows. `now` is the median of the samples of the last 7 days; with fewer than 3 of them it is the
median of the last 7 samples (a quiet series still has a number), and `prev7` is then the 7 days
before the first of those. `prev7` is the median of the 7 days before the window, None (state "new")
with fewer than 3 samples. `d30` is the 7-day window ending 30 days before now, None with fewer than 3.

State. "worse" when the change against prev7 exceeds BOTH the class's percent and absolute floors
(and "better" for the same drop); otherwise "flat". Two floors, because a percentage alone fires on
a 4 second job that moves one second and an absolute one alone ignores a slow job drifting.

Thresholds (THRESHOLDS below), chosen on 2026-10-08 from ci-history.json of the ci-metrics branch
(66 days of pushes to main, 51 week boundaries with a full week on both sides; no pull-request row
existed yet). `python3 tools/ci-summary.py --history FILE --noise` prints the week-over-week change
of the 7-day median for every series and day. That data is mostly REAL change (the build was being
restructured throughout), so two readings are used, both for the weeks around a flat stretch:

  class       week median   weekly change, weeks that moved < 10%   sampling error of a week-vs-week difference
  push_wait   320 s         at most 8.9% / 79 s (11 of 51 weeks)    median 4.0% / 18 s, p90 8.3% / 63 s  (22 pushes a week)
  ci job      163-241 s     at most 9.2% / 22 s (25 of 102)         median 3.0-4.7% / 5-15 s, p90 4.6-13% / 9-67 s
  sim job     16-946 s      at most 8.4% / 74 s, tiny job 1 s/6%    median 3.0-9.2% / 0.5-84 s (77 runs a week)
  minutes     2509 s        at most 9.9% / 222 s (2 of 10)          median 4.9% / 119 s, p90 7.1% / 144 s

(the sampling error is 1.25 * sigma / sqrt(n) per week from the weeks' own spread, sigma = 1.4826 MAD,
the two weeks combined). A flat week must stay under the floor, so the floors sit above the largest
flat-week move and at least twice the p90 sampling error: 15% / 60 s for push_wait, 20% / 20 s for a
CI job, 20% / 30 s for a simulator job (the 16 s job moves 1 s = 6% and must not fire), 15% / 240 s
for runner time. The classes with no data of their own start wider and are to be re-measured once
two weeks of rows exist: pull-request waits 25% / 90 s (one pull request's runs differ in which
paths they touch, so the spread is wider than a push's), the nightly 25% / 120 s (a ~15 minute
job that runs only for the pull requests that touch its inputs), and the daily benchmark 25% / 5 s
(a shared GitHub runner is noisier than the Mac its numbers were quoted from; the 5 s floor keeps
the 2-3 s no-op build quiet).

Labels. A benchmark row is labelled by its id (LOCAL_LABEL: "Rebuild after editing the app crate", "make check,
the whole gate", ...), not by the text a record stored, so records written before the labels were chosen read the
same; an id LOCAL_LABEL does not know keeps its record's label ("make check: cargo branch").

Order and selection (`order_metrics`). What a developer waits for goes first, trivia last:
  1. every row that is "worse", the biggest percentage first;
  2. the waits (PR CI wait, wait after merge);
  3. the local loop in the order a developer meets it (LOCAL_ORDER: edit and rebuild the app crate, the
     screens crate, the UI crate, the base crate, the unit suite, the whole gate), then any other row;
  4. runner time per push;
  5. the CI jobs, longest first.
`shown` is the default view; the rest goes under a disclosure. A CI job is shown from JOB_MIN_SECONDS (a
minute: nobody waits for a 4 second aggregator); a local part of a whole is not (the whole carries it as
`parts`); the no-op build is a health check, shown only when it climbs above HEALTH_MAX_SECONDS; a row
that is "worse" is always shown.

Comparable waits. A pull request's or a push's wait is the slower of its CI and Simulator CI runs, so a
wait is only compared where both were being recorded: a sample whose first run is earlier than the first
recorded run of both workflows for that event is left out. Without that, the 30-days-ago figure of the
pull-request wait was CI alone (a shorter thing) and read +200%. No comparable sample: the window is
empty and the row says "new".

Regressions. For a metric that is worse, `since` is the first day of the trailing run of samples that
sit above prev7 * (1 + threshold_pct / 100) (the samples are single runs, so one slow run does not
start it: the run must hold up to now), and `commits` are the push rows (sha, title, pr) of any
workflow from the day before `since` until now, at most 10, newest last. No regression: the list is empty.
"""
from __future__ import annotations

import argparse
import json
import statistics
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

SCHEMA = 2
WEEK = timedelta(days=7)
MIN_SAMPLES = 3
STALE = timedelta(days=14)          # a series whose newest sample is older than this is gone, not "now"
SLOWEST = 5
JOB_MIN_SECONDS = 60.0              # a CI job shorter than this is not something anyone waits for
HEALTH_MAX_SECONDS = 5.0            # the no-op build is a health check: listed only above this
WAITS = ("pr_wait", "push_wait")
# the benchmark's own row ids (tools/build-history.py), in the order a developer meets them
LOCAL_ORDER = ("bench.app", "bench.screens", "bench.ui", "bench.leaf", "bench.tests", "bench.check")
HEALTH_ROW = "bench.noop"
# What each benchmark row IS, in a developer's words, keyed by the row id (never by the label a record stored: the
# first records called these "app crate" and "plx_base"). An id not listed keeps the record's own label.
LOCAL_LABEL = {
    "app": "Rebuild after editing the app crate",
    "screens": "Rebuild after editing plx_screens",
    "ui": "Rebuild after editing plx_ui",
    "leaf": "Rebuild after editing plx_base",
    "tests": "Unit suite",
    "noop": "No-op build",
    "check": "make check, the whole gate",
}
KIND = {"pr_wait": "wait", "push_wait": "wait", "minutes": "cost", "bench": "local"}
MAX_COMMITS = 10
WF_LABEL = {"ci": "CI", "sim": "Simulator CI", "nightly": "Nightly"}

# class -> (percent floor, absolute floor in seconds); the module docstring says where they come from
THRESHOLDS = {
    "pr_wait": (25.0, 90.0),
    "push_wait": (15.0, 60.0),
    "ci_job": (20.0, 20.0),
    "sim_job": (20.0, 30.0),
    "nightly": (25.0, 120.0),
    "minutes": (15.0, 240.0),
    "bench": (25.0, 5.0),
}


def parse_time(text: str) -> datetime:
    return datetime.fromisoformat(text.replace("Z", "+00:00")).astimezone(timezone.utc)


def iso(dt: datetime) -> str:
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def is_green(row: dict) -> bool:
    return not row.get("conclusion") or row["conclusion"] == "success"


def row_event(row: dict) -> str:
    return row.get("event") or "push"


def row_seconds(row: dict) -> int | None:
    """What a developer waited for this run: the wall time of a re-run, else its longest job."""
    if (row.get("attempt") or 1) > 1 and row.get("wall"):
        return row["wall"]
    jobs = row.get("jobs") or {}
    return max(jobs.values()) if jobs else None


def comparable_since(history: dict, event: str, workflows: tuple) -> datetime | None:
    """The first moment every one of `workflows` that has rows for `event` was being recorded, or None
    when fewer than two do (nothing to line up). A wait from before it measured a shorter thing."""
    firsts = []
    for wf in workflows:
        ats = [parse_time(r["at"]) for r in history.get(wf) or [] if row_event(r) == event and r.get("at")]
        if ats:
            firsts.append(min(ats))
    return max(firsts) if len(firsts) > 1 else None


def commit_waits(history: dict, event: str, workflows: tuple, required: tuple = ()) -> list:
    """[(time, seconds, meta)] one per commit of `event`: its slowest run, if none of its runs is red.
    With `required`, commits first run before all of those workflows were recorded are left out."""
    floor = comparable_since(history, event, required) if required else None
    by_sha: dict = {}
    for wf in workflows:
        for row in history.get(wf) or []:
            if row_event(row) != event:
                continue
            by_sha.setdefault(row["sha"], {}).setdefault(wf, []).append(row)
    out = []
    for sha, runs in by_sha.items():
        latest = [max(rows, key=lambda r: (r["at"], r["id"])) for rows in runs.values()]     # one per workflow
        if not all(is_green(r) and row_seconds(r) is not None for r in latest):
            continue
        first = min(latest, key=lambda r: r["at"])
        if floor and parse_time(first["at"]) < floor:
            continue
        out.append((parse_time(first["at"]), max(row_seconds(r) for r in latest),
                    {"sha": sha, "title": first.get("title", ""), "pr": first.get("pr")}))
    return sorted(out, key=lambda s: s[0])


def job_samples(history: dict, wf: str, event: str) -> dict:
    """{job name: [(time, seconds, meta)]} over the green runs of one workflow and event."""
    out: dict = {}
    for row in history.get(wf) or []:
        if row_event(row) != event or not is_green(row):
            continue
        for name, secs in (row.get("jobs") or {}).items():
            out.setdefault(name, []).append((parse_time(row["at"]), secs, {"sha": row["sha"], "title": row.get("title", ""), "pr": row.get("pr")}))
    return {n: sorted(v, key=lambda s: s[0]) for n, v in out.items()}


def all_series(history: dict, bench: dict | None) -> dict:
    """id -> {label, group, cls, samples}, derived from the rows; no job is named here."""
    series: dict = {}

    def add(sid, label, group, cls, samples):
        if samples:
            series[sid] = {"label": label, "group": group, "cls": cls, "samples": samples}

    add("pr_wait", "PR CI wait", "ci", "pr_wait", commit_waits(history, "pull_request", ("ci", "sim", "nightly"), required=("ci", "sim")))
    add("push_wait", "Wait after merge", "ci", "push_wait", commit_waits(history, "push", ("ci", "sim"), required=("ci", "sim")))
    for wf, event in (("ci", "push"), ("sim", "push"), ("nightly", "pull_request")):
        for name, samples in job_samples(history, wf, event).items():
            add(f"{wf}|{name}", f"{WF_LABEL[wf]}: {name}", "ci", {"ci": "ci_job", "sim": "sim_job", "nightly": "nightly"}[wf], samples)
    by_sha: dict = {}
    for wf in ("ci", "sim"):
        for row in history.get(wf) or []:
            if row_event(row) == "push" and is_green(row) and row.get("jobs"):
                by_sha.setdefault(row["sha"], {})[wf] = row
    minutes = [(parse_time(min(r["at"] for r in runs.values())), sum(sum(r["jobs"].values()) for r in runs.values()),
                {"sha": sha, "title": next(iter(runs.values())).get("title", ""), "pr": next(iter(runs.values())).get("pr")})
               for sha, runs in by_sha.items() if len(runs) == 2]
    add("minutes", "Runner time per push", "ci", "minutes", sorted(minutes, key=lambda s: s[0]))
    rows: dict = {}
    for rec in (bench or {}).get("records") or []:
        for rid, r in (rec.get("rows") or {}).items():
            if isinstance(r.get("median"), (int, float)):
                at = parse_time(rec.get("at") or rec["date"] + "T00:00:00Z")
                rows.setdefault(rid, {"label": LOCAL_LABEL.get(rid) or r.get("label") or rid, "samples": []})["samples"].append(
                    (at, r["median"], {"sha": rec.get("commit", ""), "title": "", "pr": None}))
    for rid, r in rows.items():
        add(f"bench.{rid}", r["label"], "local", "bench", sorted(r["samples"], key=lambda s: s[0]))
    return series


def med(values):
    return statistics.median(values) if values else None


def window(samples: list, lo: datetime, hi: datetime) -> list:
    return [s for s in samples if lo < s[0] <= hi]


def windows(samples: list, now: datetime) -> dict:
    """The three medians and their sample counts (see the module docstring)."""
    cur = window(samples, now - WEEK, now)
    start = now - WEEK
    if len(cur) < MIN_SAMPLES:
        cur = [s for s in samples if s[0] <= now][-7:]
        start = cur[0][0] - timedelta(microseconds=1) if cur else start
    prev = window(samples, start - WEEK, start)
    d30 = window(samples, now - timedelta(days=37), now - timedelta(days=30))
    return {"now": med([s[1] for s in cur]), "n_now": len(cur),
            "prev7": med([s[1] for s in prev]) if len(prev) >= MIN_SAMPLES else None, "n_prev": len(prev),
            "d30": med([s[1] for s in d30]) if len(d30) >= MIN_SAMPLES else None, "start": start}


def pct(now, before):
    return None if before in (None, 0) or now is None else round(100.0 * (now - before) / before, 1)


def state_of(now, prev, pct_floor, abs_floor) -> str:
    if prev is None or now is None:
        return "new"
    delta = now - prev
    if prev and 100.0 * delta / prev > pct_floor and delta > abs_floor:
        return "worse"
    if prev and 100.0 * -delta / prev > pct_floor and -delta > abs_floor:
        return "better"
    return "flat"


def regression_since(samples: list, now: datetime, prev7: float, pct_floor: float) -> str:
    """The first day of the trailing run of samples above prev7 * (1 + pct_floor/100)."""
    limit = prev7 * (1 + pct_floor / 100.0)
    live = [s for s in samples if s[0] <= now]
    first = None
    for s in reversed(live):
        if s[1] > limit:
            first = s
        else:
            break
    return (first or live[-1])[0].strftime("%Y-%m-%d")


def commits_since(history: dict, since: str, now: datetime) -> list:
    lo = parse_time(since + "T00:00:00Z") - timedelta(days=1)
    seen, out = set(), []
    for wf in ("ci", "sim"):
        for row in history.get(wf) or []:
            if row_event(row) == "push" and lo <= parse_time(row["at"]) <= now and row["sha"] not in seen:
                seen.add(row["sha"])
                out.append((row["at"], {"sha": row["sha"], "title": row.get("title", ""), "pr": row.get("pr")}))
    return [c for _, c in sorted(out, key=lambda t: t[0])][-MAX_COMMITS:]


def summarize(history: dict, bench: dict | None, now: datetime) -> dict:
    metrics, regressions = [], []
    for sid, se in sorted(all_series(history, bench).items()):
        samples = se["samples"]
        if samples[-1][0] < now - STALE:
            continue
        w = windows(samples, now)
        pct_floor, abs_floor = THRESHOLDS[se["cls"]]
        state = state_of(w["now"], w["prev7"], pct_floor, abs_floor)
        m = {"id": sid, "label": se["label"], "group": se["group"], "kind": KIND.get(se["cls"], "job"), "unit": "s",
             "now": round(w["now"], 1), "prev7": None if w["prev7"] is None else round(w["prev7"], 1),
             "d30": None if w["d30"] is None else round(w["d30"], 1),
             "delta7": None if w["prev7"] is None else round(w["now"] - w["prev7"], 1),
             "delta30": None if w["d30"] is None else round(w["now"] - w["d30"], 1),
             "change7_pct": pct(w["now"], w["prev7"]), "change30_pct": pct(w["now"], w["d30"]),
             "state": state, "state30": state_of(w["now"], w["d30"], pct_floor, abs_floor),
             "threshold_pct": pct_floor, "threshold_abs": abs_floor,
             "n_now": w["n_now"], "n_prev": w["n_prev"]}
        metrics.append(m)
        if state == "worse":
            since = regression_since(samples, now, w["prev7"], pct_floor)
            regressions.append({"id": sid, "label": se["label"], "since": since, "from": m["prev7"], "to": m["now"],
                                "change_pct": m["change7_pct"], "threshold_pct": pct_floor,
                                "commits": commits_since(history, since, now)})
    link_parts(metrics)
    metrics = order_metrics(metrics)
    top = lambda ms: [{"id": m["id"], "label": m["label"], "now": m["now"], "group": m["group"]} for m in ms[:SLOWEST]]
    longest = lambda ms: sorted(ms, key=lambda m: (-m["now"], m["id"]))
    return {"schema": SCHEMA, "generated": iso(now),
            "thresholds": {k: {"percent": p, "seconds": a} for k, (p, a) in sorted(THRESHOLDS.items())},
            "metrics": metrics,
            "slowest_ci": top(longest([m for m in metrics if m["kind"] == "job"])),
            "slowest_local": top(longest([m for m in metrics if m["kind"] == "local" and "parent" not in m and m["id"] != HEALTH_ROW])),
            "regressions": sorted(regressions, key=lambda r: (-r["change_pct"], r["id"]))}


def link_parts(metrics: list) -> None:
    """A local row `bench.check.cargo` is a part of `bench.check` when that row exists: the part gets
    `parent`, the whole gets `parts` (largest first). They are one measurement, never two lines of a list."""
    by_id = {m["id"]: m for m in metrics}
    for m in metrics:
        parent = m["id"].rsplit(".", 1)[0] if m["kind"] == "local" and m["id"].count(".") > 1 else None
        if parent in by_id:
            m["parent"] = parent
            by_id[parent].setdefault("parts", []).append({"id": m["id"], "label": m["label"], "now": m["now"]})
    for m in metrics:
        if "parts" in m:
            m["parts"].sort(key=lambda p: (-p["now"], p["id"]))


def is_shown(m: dict) -> bool:
    """The default view: see "Order and selection" in the module docstring."""
    if m["state"] == "worse":
        return True
    if m["kind"] == "job":
        return m["now"] >= JOB_MIN_SECONDS
    if m["kind"] == "local":
        if "parent" in m:
            return False
        return m["now"] > HEALTH_MAX_SECONDS if m["id"] == HEALTH_ROW else True
    return True


def order_metrics(metrics: list) -> list:
    """Worse first, then the waits, the local loop, runner time, the jobs longest first; `shown` says which
    of them are the default view (they all come before the rest)."""
    for m in metrics:
        m["shown"] = is_shown(m)

    def key(m):
        if not m["shown"]:
            return (6, 0, -m["now"], m["id"])
        if m["state"] == "worse":
            return (0, 0, -(m["change7_pct"] or 0), m["id"])
        if m["kind"] == "wait":
            return (1, WAITS.index(m["id"]), 0, m["id"])
        if m["kind"] == "local":
            return (2, LOCAL_ORDER.index(m["id"]) if m["id"] in LOCAL_ORDER else len(LOCAL_ORDER), -m["now"], m["id"])
        if m["kind"] == "cost":
            return (3, 0, 0, m["id"])
        return (4, 0, -m["now"], m["id"])
    return sorted(metrics, key=key)


def noise_report(history: dict, bench: dict | None, now: datetime) -> dict:
    """Per class: the week-over-week change (%) of the 7-day median between consecutive weeks, every week with both sides full."""
    out: dict = {}
    for sid, se in all_series(history, bench).items():
        samples, first, last = se["samples"], se["samples"][0][0], se["samples"][-1][0]
        edge = last
        changes = []
        while edge - 2 * WEEK >= first:
            cur, prev = window(samples, edge - WEEK, edge), window(samples, edge - 2 * WEEK, edge - WEEK)
            if len(cur) >= MIN_SAMPLES and len(prev) >= MIN_SAMPLES:
                a, b = med([s[1] for s in cur]), med([s[1] for s in prev])
                changes.append((edge.strftime("%Y-%m-%d"), round(100.0 * (a - b) / b, 1), round(a - b, 1)))
            edge -= timedelta(days=1)       # overlapping steps: every day is a week boundary once
        if changes:
            out.setdefault(se["cls"], {})[sid] = changes
    return out


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--history", required=True, help="ci-history.json")
    ap.add_argument("--bench", help="build-history.json (optional)")
    ap.add_argument("--out", help="where to write ci-summary.json")
    ap.add_argument("--now", help="ISO time to compute for (default: the clock)")
    ap.add_argument("--noise", action="store_true", help="print the week-over-week noise per class and write nothing")
    args = ap.parse_args(argv)
    if not args.out and not args.noise:
        ap.error("--out is required unless --noise")
    now = parse_time(args.now) if args.now else datetime.now(timezone.utc).replace(microsecond=0)
    try:
        history = json.loads(Path(args.history).read_text(encoding="utf-8") or "{}")
        bench = json.loads(Path(args.bench).read_text(encoding="utf-8")) if args.bench and Path(args.bench).is_file() else None
    except (OSError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    if args.noise:
        print(json.dumps(noise_report(history, bench, now), indent=1))
        return 0
    doc = summarize(history, bench, now)
    tmp = f"{args.out}.tmp"
    Path(tmp).write_text(json.dumps(doc, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    Path(tmp).replace(args.out)
    worse = [m["id"] for m in doc["metrics"] if m["state"] == "worse"]
    print(f"ci-summary: {len(doc['metrics'])} metrics, {len(worse)} worse{': ' + ', '.join(worse) if worse else ''}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
