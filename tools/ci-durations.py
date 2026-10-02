#!/usr/bin/env python3
"""CI duration trends: which job got slower, and which step is the slow one.

usage: tools/ci-durations.py [--runs 30] [--recent 5] [--top-steps 8] [--repo OWNER/NAME]
                             [--workflow NAME ...] [--json]

Reads the last N completed SUCCESSFUL runs on `main` of the workflows CI and Simulator CI through
`gh api` (needs `gh auth login`; read-only), and prints per job the median and p90 duration over
all N runs, then compares the median of the most recent 5 runs with the median of the 25 before
them. A job is flagged GROWN when its recent median is more than 20% AND more than 30 s above the
earlier one: the relative bar ignores the cheap jobs' jitter, the absolute bar ignores a 6 s job
that went to 8 s. Below the job table, the slowest steps (by median over the window) show where
the time sits.

Wall-clock numbers on shared runners are noisy (the apt step alone has taken 132 to 1174 s), so a
flag is a prompt to open the job's log and the cargo `--timings` artifact, not a verdict. The
deterministic gates (sizes, counts) are ci/check-build-budgets.py; this is the trend reader.

Run it on demand, or have an agent run it before and after a change to the build. Not wired into
CI. `--json` prints the same data as one JSON document for scripts.
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
import subprocess
import sys
from datetime import datetime

DEFAULT_REPO = "GLinnik21/plx-native"
# workflow display name -> workflow file (the API addresses a workflow by either)
WORKFLOWS = {"CI": "ci.yml", "Simulator CI": "simulators.yml"}
GROWTH_RATIO = 0.20     # recent median more than 20% above the earlier median ...
GROWTH_SECONDS = 30.0   # ... and more than this many seconds above it


class ApiError(Exception):
    pass


def gh_api(path: str):
    """GET one API path through the gh CLI and return the decoded JSON."""
    try:
        out = subprocess.run(["gh", "api", path], check=True, capture_output=True, text=True,
                             timeout=120).stdout
    except (OSError, subprocess.SubprocessError) as exc:
        detail = getattr(exc, "stderr", "") or exc
        raise ApiError(f"gh api {path} failed: {detail}") from exc
    return json.loads(out)


def parse_time(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def seconds(item: dict) -> float | None:
    """Duration of a job or step, or None when it did not run to a successful end."""
    if item.get("conclusion") != "success" or not item.get("started_at") or not item.get("completed_at"):
        return None
    return (parse_time(item["completed_at"]) - parse_time(item["started_at"])).total_seconds()


def median(values: list[float]) -> float | None:
    return statistics.median(values) if values else None


def p90(values: list[float]) -> float | None:
    """Nearest-rank 90th percentile (no interpolation, so it is always a duration that happened)."""
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(math.ceil(0.9 * len(ordered)) - 1, 0)]


def fetch_runs(api, repo: str, workflow_file: str, count: int) -> list[dict]:
    """Newest-first successful runs on main, each with its jobs."""
    listing = api(f"repos/{repo}/actions/workflows/{workflow_file}/runs"
                  f"?branch=main&status=success&per_page={count}")
    runs = sorted(listing.get("workflow_runs", []), key=lambda r: r["created_at"], reverse=True)[:count]
    out = []
    for run in runs:
        jobs = api(f"repos/{repo}/actions/runs/{run['id']}/jobs?per_page=100&filter=latest")
        out.append({"id": run["id"], "created_at": run["created_at"], "jobs": jobs.get("jobs", [])})
    return out


def analyse(runs: list[dict], recent: int, top_steps: int) -> dict:
    """Per-job and per-step statistics for runs given newest first."""
    durations: dict[str, list[float]] = {}     # job -> durations, newest first
    steps: dict[tuple[str, str], list[float]] = {}
    for run in runs:
        for job in run["jobs"]:
            d = seconds(job)
            if d is None:
                continue
            durations.setdefault(job["name"], []).append(d)
            for step in job.get("steps", []):
                sd = seconds(step)
                if sd is not None:
                    steps.setdefault((job["name"], step["name"]), []).append(sd)
    jobs = []
    for name, values in sorted(durations.items()):
        new, old = values[:recent], values[recent:]
        new_m, old_m = median(new), median(old)
        grown = (new_m is not None and old_m is not None
                 and new_m > old_m * (1 + GROWTH_RATIO) and new_m - old_m > GROWTH_SECONDS)
        jobs.append({
            "job": name, "runs": len(values),
            "median_s": median(values), "p90_s": p90(values),
            "recent_runs": len(new), "recent_median_s": new_m,
            "previous_runs": len(old), "previous_median_s": old_m,
            "change_s": None if new_m is None or old_m is None else new_m - old_m,
            "change_pct": None if new_m is None or not old_m else (new_m - old_m) / old_m * 100,
            "grown": grown,
        })
    slow = sorted(({"job": j, "step": s, "runs": len(v), "median_s": median(v)} for (j, s), v in steps.items()),
                  key=lambda r: r["median_s"], reverse=True)[:top_steps]
    return {"runs_used": len(runs), "recent": recent, "jobs": jobs, "slowest_steps": slow}


def fmt(sec: float | None) -> str:
    if sec is None:
        return "n/a"
    if sec >= 90:
        return f"{int(sec // 60)}m{int(round(sec % 60)):02d}s"
    return f"{sec:.0f}s"


def render(report: dict) -> str:
    lines = []
    for wf, data in report["workflows"].items():
        lines += [f"### {wf} ({data['runs_used']} successful runs on main; recent = newest {data['recent']})", "",
                  "| job | runs | median | p90 | recent median | previous median | change | flag |",
                  "|---|---:|---:|---:|---:|---:|---:|---|"]
        for j in data["jobs"]:
            change = "n/a" if j["change_s"] is None else f"{j['change_s']:+.0f}s ({j['change_pct']:+.0f}%)"
            lines.append(f"| {j['job']} | {j['runs']} | {fmt(j['median_s'])} | {fmt(j['p90_s'])} | "
                         f"{fmt(j['recent_median_s'])} | {fmt(j['previous_median_s'])} | {change} | "
                         f"{'GROWN' if j['grown'] else ''} |")
        lines += ["", f"Slowest steps of {wf} (median over the window):", "",
                  "| job | step | runs | median |", "|---|---|---:|---:|"]
        lines += [f"| {s['job']} | {s['step']} | {s['runs']} | {fmt(s['median_s'])} |" for s in data["slowest_steps"]]
        lines.append("")
    grown = [f"{wf}: {j['job']}" for wf, d in report["workflows"].items() for j in d["jobs"] if j["grown"]]
    lines.append("Grown (recent median > +20% and > +30 s): " + ("; ".join(grown) if grown else "none"))
    return "\n".join(lines)


def build_report(api, repo: str, workflows: dict[str, str], count: int, recent: int, top_steps: int) -> dict:
    report = {"repo": repo, "workflows": {}}
    for name, file in workflows.items():
        report["workflows"][name] = analyse(fetch_runs(api, repo, file, count), recent, top_steps)
    return report


def main(argv=None, api=gh_api) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--runs", type=int, default=30, help="completed successful runs per workflow (default 30)")
    ap.add_argument("--recent", type=int, default=5, help="runs in the recent window (default 5)")
    ap.add_argument("--top-steps", type=int, default=8, help="slowest steps to list per workflow (default 8)")
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--workflow", action="append", choices=sorted(WORKFLOWS),
                    help="limit to a workflow (repeatable); default: both")
    ap.add_argument("--json", action="store_true", help="print the data as JSON instead of Markdown")
    args = ap.parse_args(argv)
    if not 1 <= args.recent < args.runs <= 100:
        print("error: need 1 <= --recent < --runs <= 100", file=sys.stderr)
        return 2
    workflows = {n: WORKFLOWS[n] for n in (args.workflow or WORKFLOWS)}
    try:
        report = build_report(api, args.repo, workflows, args.runs, args.recent, args.top_steps)
    except ApiError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(report, indent=2) if args.json else render(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
