#!/usr/bin/env python3
"""Incrementally maintain ci-history.json: one row per successful `main` push run, per workflow.

usage: tools/ci-history.py --in FILE --out FILE [--repo OWNER/NAME] [--max-pages N] [--full] [--workers N]

This is the data behind the live chart at https://plxnative.com/ci/ (site/ci/index.html). The
`ci-metrics` workflow runs it after every CI / Simulator CI run on main and commits the result to
the orphan `ci-metrics` branch; the page fetches that file from raw.githubusercontent.com.

The file is `{"updated": ISO time, "ci": [row...], "sim": [row...]}`, each row
`{"id", "at" (run_started_at), "sha" (9 chars), "title" (display_title, first 90 chars),
"jobs": {job name: seconds}}`, sorted by `at`. `ci` is ci.yml, `sim` is simulators.yml. Only runs
with conclusion `success` and event `push` are kept, and only jobs that themselves succeeded.

Incremental: `--in` is loaded first (a missing or empty file, e.g. /dev/null, starts empty, which
backfills everything reachable by paging); runs are listed newest first and paging stops at the
first page whose kept runs are all already present (`--full` pages to the end). Job timings are fetched for new runs only.
Idempotent: a second run with nothing new rewrites byte-identical output (`updated` only moves
when a row is added), so the workflow can skip the commit.

Filtering happens here, not in the query: the `status=success` query parameter of the runs listing
was verified stale on 2026-10-02 (it dropped every run after 2026-10-01). Needs `gh` and a token
(GH_TOKEN in CI); read-only.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import sys
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path

# Reuse the gh wrapper and time parsing of the trend reader rather than copying them.
_spec = importlib.util.spec_from_file_location("ci_durations", Path(__file__).with_name("ci-durations.py"))
_durations = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_durations)
ApiError = _durations.ApiError
gh_api = _durations.gh_api
parse_time = _durations.parse_time
DEFAULT_REPO = _durations.DEFAULT_REPO

# series key in the JSON -> workflow file
WORKFLOWS = {"ci": "ci.yml", "sim": "simulators.yml"}
TITLE_MAX = 90
SHA_LEN = 9
PER_PAGE = 100


def load(path: str) -> dict:
    """The existing document, or an empty one for a missing, empty or unreadable-as-JSON-empty file."""
    try:
        text = Path(path).read_text(encoding="utf-8")
    except OSError:
        text = ""
    doc = json.loads(text) if text.strip() else {}
    return {"updated": doc.get("updated"), **{k: list(doc.get(k) or []) for k in WORKFLOWS}}


def keep(run: dict) -> bool:
    return run.get("conclusion") == "success" and run.get("event") == "push"


def job_seconds(jobs: list[dict]) -> dict[str, int]:
    out = {}
    for job in jobs:
        if job.get("conclusion") == "success" and job.get("started_at") and job.get("completed_at"):
            out[job["name"]] = round((parse_time(job["completed_at"]) - parse_time(job["started_at"])).total_seconds())
    return out


def new_runs(api, repo: str, workflow_file: str, known: set, max_pages: int, full: bool = False) -> list[dict]:
    """Kept runs not in `known`, newest first, stopping at the first page whose kept runs are all known.

    `full` keeps paging to the end (or `max_pages`) regardless. The listing is not a stable snapshot:
    the first live backfill saw the same run on two pages and missed 45 others, so a run can be
    repeated here and an incremental pass can leave a gap that only a full pass closes.
    """
    found = []
    seen = set(known)
    for page in range(1, max_pages + 1):
        runs = api(f"repos/{repo}/actions/workflows/{workflow_file}/runs"
                   f"?branch=main&per_page={PER_PAGE}&page={page}").get("workflow_runs", [])
        kept = [r for r in runs if keep(r)]
        fresh = [r for r in kept if r["id"] not in known]
        found += [r for r in fresh if r["id"] not in seen]
        seen.update(r["id"] for r in fresh)
        if not runs or (kept and not fresh and not full):
            break
    return found


def row_for(api, repo: str, run: dict) -> dict:
    jobs = api(f"repos/{repo}/actions/runs/{run['id']}/jobs?per_page=100").get("jobs", [])
    return {"id": run["id"], "at": run.get("run_started_at") or run["created_at"],
            "sha": run["head_sha"][:SHA_LEN], "title": (run.get("display_title") or "")[:TITLE_MAX],
            "jobs": job_seconds(jobs)}


def update(doc: dict, api, repo: str, max_pages: int, workers: int, now: str, full: bool = False) -> int:
    """Merge new runs into `doc` in place; returns how many rows were added."""
    added = 0
    for key, wf in WORKFLOWS.items():
        known = {r["id"] for r in doc[key]}
        fresh = new_runs(api, repo, wf, known, max_pages, full)
        with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
            rows = list(pool.map(lambda r: row_for(api, repo, r), fresh))
        unique = {r["id"]: r for r in doc[key] + rows}      # also heals duplicates an older file may hold
        doc[key] = sorted(unique.values(), key=lambda r: (r["at"], r["id"]))
        added += len(rows)
    if added or not doc.get("updated"):
        doc["updated"] = now
    return added


def dump(doc: dict) -> str:
    """One row per line, so a commit to the data branch diffs as the rows it added."""
    def block(rows):
        return "[\n" + ",\n".join(json.dumps(r, separators=(",", ":")) for r in rows) + "\n]" if rows else "[]"
    return (f'{{"updated":{json.dumps(doc["updated"])},\n"ci":{block(doc["ci"])},\n'
            f'"sim":{block(doc["sim"])}}}\n')


def main(argv=None, api=gh_api, now=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--in", dest="src", required=True, help="existing ci-history.json (missing or empty = start fresh)")
    ap.add_argument("--out", required=True, help="where to write the merged file")
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--max-pages", type=int, default=30, help="run-list pages (100 runs each) per workflow (default 30)")
    ap.add_argument("--full", action="store_true", help="page to the end instead of stopping at the first known page (a backfill / gap repair)")
    ap.add_argument("--workers", type=int, default=8, help="parallel job-timing fetches (default 8)")
    args = ap.parse_args(argv)
    now = now or datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    try:
        doc = load(args.src)
        added = update(doc, api, args.repo, args.max_pages, args.workers, now, args.full)
    except (ApiError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    tmp = f"{args.out}.tmp"
    Path(tmp).write_text(dump(doc), encoding="utf-8")
    os.replace(tmp, args.out)
    print(f"ci-history: +{added} rows; ci={len(doc['ci'])} sim={len(doc['sim'])}; updated {doc['updated']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
