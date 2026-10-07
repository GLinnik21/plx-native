#!/usr/bin/env python3
"""Incrementally maintain ci-history.json: one row per finished `main` push run, per workflow.

usage: tools/ci-history.py --in FILE (--out FILE | --dry-run) [--repo OWNER/NAME] [--max-pages N] [--full] [--workers N]

This is the data behind the live chart at https://plxnative.com/ci/ (site/ci/index.html). The
`ci-metrics` workflow runs it after every CI / Simulator CI run on main and commits the result to
the orphan `ci-metrics` branch; the page fetches that file from raw.githubusercontent.com.

The file is `{"updated": ISO time, "ci": [row...], "sim": [row...]}`, each row
`{"id", "at" (run_started_at), "sha" (9 chars), "title" (the commit subject without its
trailing "(#N)", first 120 chars), "pr" (the N of that trailing "(#N)", the merge PR, or null),
"conclusion" ("success", "failure" or "timed_out"), "attempt" (run_attempt: 2 or more means the
run was re-run), "jobs": {job name: seconds}, "failed": [job names]}`, sorted by `at`. `ci` is
ci.yml, `sim` is simulators.yml. Only runs of event `push` that finished `success`, `failure` or
`timed_out` are kept (a cancelled run is the concurrency group superseding it, not a verdict).
`jobs` holds only jobs that themselves succeeded, so a failed run never drags a job's duration
series down with a job that stopped early; `failed` names the jobs that did not and is left out
when empty. Rows written before `conclusion` and `attempt` existed lack both: they were only ever
written for successful runs, and the page reads them that way.

Incremental: `--in` is loaded first (a missing or empty file, e.g. /dev/null, starts empty, which
backfills everything reachable by paging); runs are listed newest first and paging stops at the
first page whose kept runs are all already present and unchanged (`--full` pages to the end, and
also refreshes `title`, `pr`, `conclusion` and `attempt` of rows already stored from the run
listing, which carries them, so rows written by an older version of this tool gain them). A stored
row whose run was re-run since (a higher `attempt`, or a different `conclusion`) is recorded
again, because its job timings belong to the attempt that is now the run's. Job timings are
fetched for new or changed runs only. Idempotent: a second run with nothing new rewrites
byte-identical output (`updated` only moves when a row is added or re-recorded), so the workflow
can skip the commit.

`--dry-run` does everything except write: it prints how many rows a real run would add and re-record
(and the time span they cover) and leaves `--out` alone, so a backfill can be sized from a laptop.

Filtering happens here, not in the query: the `status=success` query parameter of the runs listing
was verified stale on 2026-10-02 (it dropped every run after 2026-10-01). Needs `gh` and a token
(GH_TOKEN in CI); read-only.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
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
TITLE_MAX = 120
PR_SUFFIX = re.compile(r"\s*\(#(\d+)\)\s*$")
SHA_LEN = 9
PER_PAGE = 100
FAILED = ("failure", "timed_out")
KEPT = ("success",) + FAILED


def load(path: str) -> dict:
    """The existing document, or an empty one for a missing, empty or unreadable-as-JSON-empty file."""
    try:
        text = Path(path).read_text(encoding="utf-8")
    except OSError:
        text = ""
    doc = json.loads(text) if text.strip() else {}
    return {"updated": doc.get("updated"), **{k: list(doc.get(k) or []) for k in WORKFLOWS}}


def keep(run: dict) -> bool:
    return run.get("conclusion") in KEPT and run.get("event") == "push"


def signature(run: dict) -> tuple:
    """What a re-run changes about a run: its verdict and its attempt number."""
    return (run.get("conclusion"), run.get("run_attempt") or 1)


def job_seconds(jobs: list[dict]) -> dict[str, int]:
    out = {}
    for job in jobs:
        if job.get("conclusion") == "success" and job.get("started_at") and job.get("completed_at"):
            out[job["name"]] = round((parse_time(job["completed_at"]) - parse_time(job["started_at"])).total_seconds())
    return out


def failed_jobs(jobs: list[dict]) -> list[str]:
    return sorted({job["name"] for job in jobs if job.get("conclusion") in FAILED})


def split_title(display_title: str) -> tuple[str, int | None]:
    """(title, pr): the last trailing "(#N)" is the merge PR and moves out of the title, which is then cut.

    Split before cutting: a long title would otherwise lose its "(#N)" to the cut.
    "Build: x (#329) (#343)" -> ("Build: x (#329)", 343).
    """
    text = display_title or ""
    m = PR_SUFFIX.search(text)
    if m:
        return text[:m.start()][:TITLE_MAX], int(m.group(1))
    return text[:TITLE_MAX], None


def subject(run: dict) -> str:
    """The commit subject of a run: the first line of head_commit.message, else display_title.

    The API cuts display_title at about 70 characters and ends it with an ellipsis, which drops the
    trailing "(#N)" of a long squash-merge title before this tool ever sees it. The listing's
    head_commit.message is whole, so it is the source and display_title only the fallback.
    """
    message = ((run.get("head_commit") or {}).get("message") or "").strip()
    return message.splitlines()[0].strip() if message else (run.get("display_title") or "")


def new_runs(api, repo: str, workflow_file: str, known: dict, max_pages: int, full: bool = False,
             listed: dict | None = None) -> list[dict]:
    """Kept runs that are new or changed, newest first, stopping at the first page with none.

    `known` maps a stored run id to its (conclusion, attempt), or to None for a row written before
    those were recorded; such a row is never "changed" (only `full` upgrades it, from `listed`).
    `listed`, when given, is filled with every kept run seen, by id (known ones included).

    `full` keeps paging to the end (or `max_pages`) regardless. The listing is not a stable snapshot:
    the first live backfill saw the same run on two pages and missed 45 others, so a run can be
    repeated here and an incremental pass can leave a gap that only a full pass closes.
    """
    found = []
    seen = set()
    for page in range(1, max_pages + 1):
        runs = api(f"repos/{repo}/actions/workflows/{workflow_file}/runs"
                   f"?branch=main&per_page={PER_PAGE}&page={page}").get("workflow_runs", [])
        kept = [r for r in runs if keep(r)]
        if listed is not None:
            listed.update((r["id"], r) for r in kept)
        fresh = [r for r in kept if r["id"] not in known
                 or (known[r["id"]] is not None and known[r["id"]] != signature(r))]
        found += [r for r in fresh if r["id"] not in seen]
        seen.update(r["id"] for r in fresh)
        if not runs or (kept and not fresh and not full):
            break
    return found


def row_for(api, repo: str, run: dict) -> dict:
    jobs = api(f"repos/{repo}/actions/runs/{run['id']}/jobs?per_page=100").get("jobs", [])
    title, pr = split_title(subject(run))
    row = {"id": run["id"], "at": run.get("run_started_at") or run["created_at"],
           "sha": run["head_sha"][:SHA_LEN], "title": title, "pr": pr,
           "conclusion": run["conclusion"], "attempt": run.get("run_attempt") or 1, "jobs": job_seconds(jobs)}
    failed = failed_jobs(jobs)
    if failed:
        row["failed"] = failed
    return row


def update(doc: dict, api, repo: str, max_pages: int, workers: int, now: str, full: bool = False) -> tuple[list, list]:
    """Merge new and re-run rows into `doc` in place; returns (added, refreshed) rows."""
    added, refreshed = [], []
    for key, wf in WORKFLOWS.items():
        known = {r["id"]: ((r["conclusion"], r.get("attempt") or 1) if r.get("conclusion") else None) for r in doc[key]}
        listed: dict = {}
        fresh = new_runs(api, repo, wf, known, max_pages, full, listed)
        if full:        # the listing carries all of this, so stored rows are refreshed without refetching jobs
            refetched = {f["id"] for f in fresh}
            for r in doc[key]:
                run = listed.get(r["id"])
                if run and r["id"] not in refetched:
                    r["title"], r["pr"] = split_title(subject(run))
                    r["conclusion"], r["attempt"] = signature(run)
        with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
            rows = list(pool.map(lambda r: row_for(api, repo, r), fresh))
        unique = {r["id"]: r for r in doc[key] + rows}      # also heals duplicates an older file may hold
        doc[key] = sorted(unique.values(), key=lambda r: (r["at"], r["id"]))
        added += [r for r in rows if r["id"] not in known]
        refreshed += [r for r in rows if r["id"] in known]
    if added or refreshed or not doc.get("updated"):
        doc["updated"] = now
    return added, refreshed


def dump(doc: dict) -> str:
    """One row per line, so a commit to the data branch diffs as the rows it added."""
    def block(rows):
        return "[\n" + ",\n".join(json.dumps(r, separators=(",", ":")) for r in rows) + "\n]" if rows else "[]"
    return (f'{{"updated":{json.dumps(doc["updated"])},\n"ci":{block(doc["ci"])},\n'
            f'"sim":{block(doc["sim"])}}}\n')


def main(argv=None, api=gh_api, now=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--in", dest="src", required=True, help="existing ci-history.json (missing or empty = start fresh)")
    ap.add_argument("--out", help="where to write the merged file")
    ap.add_argument("--dry-run", action="store_true", help="count what would be added and write nothing")
    ap.add_argument("--repo", default=DEFAULT_REPO)
    ap.add_argument("--max-pages", type=int, default=30, help="run-list pages (100 runs each) per workflow (default 30)")
    ap.add_argument("--full", action="store_true", help="page to the end instead of stopping at the first known page (a backfill / gap repair)")
    ap.add_argument("--workers", type=int, default=8, help="parallel job-timing fetches (default 8)")
    args = ap.parse_args(argv)
    if not args.out and not args.dry_run:
        ap.error("--out is required unless --dry-run")
    now = now or datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    try:
        doc = load(args.src)
        added, refreshed = update(doc, api, args.repo, args.max_pages, args.workers, now, args.full)
    except (ApiError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    if args.dry_run:
        span = sorted(r["at"] for r in added)
        print(f"ci-history: dry run, nothing written: would add {len(added)} rows "
              f"(ci={sum(r in doc['ci'] for r in added)} sim={sum(r in doc['sim'] for r in added)}"
              f"{', ' + span[0] + ' .. ' + span[-1] if span else ''}) and re-record {len(refreshed)}")
        return 0
    tmp = f"{args.out}.tmp"
    Path(tmp).write_text(dump(doc), encoding="utf-8")
    os.replace(tmp, args.out)
    print(f"ci-history: +{len(added)} rows, {len(refreshed)} re-recorded; ci={len(doc['ci'])} sim={len(doc['sim'])}; updated {doc['updated']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
