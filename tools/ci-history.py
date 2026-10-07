#!/usr/bin/env python3
"""Incrementally maintain ci-history.json: one row per finished run of CI, Simulator CI and the nightly.

usage: tools/ci-history.py --in FILE (--out FILE | --dry-run) [--repo OWNER/NAME] [--max-pages N] [--full] [--workers N]

This is the data behind the live page at https://plxnative.com/ci/ (site/ci/index.html). The
`ci-metrics` workflow runs it after every CI / Simulator CI / Nightly run (a push to main or a pull
request) and commits the result to the orphan `ci-metrics` branch; the page fetches that file from
raw.githubusercontent.com. tools/ci-summary.py derives the page's "now" block from it.

The file is `{"updated": ISO time, "ci": [row...], "sim": [row...], "nightly": [row...],
"cancelled": {day: {"push": n, "pull_request": n}}, "daily": [entry...]}`. A row is
`{"id", "at" (run_started_at), "sha" (9 chars), "title" (the commit subject without its
trailing "(#N)", first 120 chars), "pr", "event" (only on a pull request's run), "conclusion"
("success", "failure" or "timed_out"), "attempt" (run_attempt: 2 or more means the run was re-run),
"jobs": {job name: seconds}, "wall" (only when attempt > 1), "failed": [job names]}`, sorted by `at`.
`ci` is ci.yml, `sim` is simulators.yml, `nightly` is nightly.yml. A row without `event` is a push to
main; for a push `pr` is the N of the trailing "(#N)" of the subject (the merge PR, or null); for a
`pull_request` row it is the pull request's number (from the run's `pull_requests`, which the API
fills only while the PR is open, else from one `pulls?head=owner:branch` lookup per head branch; null when
neither knows) and `sha` is the PR's head commit. The nightly is recorded for pull requests only (its
schedule runs are releases). Only runs that finished `success`, `failure` or `timed_out` are kept:
a cancelled run is the concurrency group superseding it, not a verdict, so it is COUNTED per day
in `cancelled` (pushes and pull requests, all workflows summed) and has no row. `jobs` holds only
jobs that themselves succeeded, so a failed run never drags a job's duration series down with a job
that stopped early; `failed` names the jobs that did not and is left out when empty. `wall` is
seconds from the run's created_at to its last update: for a run that was re-run it is what the
developer waited across the attempts (the jobs only hold the last attempt's timings). Rows written
before `conclusion` and `attempt` existed lack both: they were only ever written for successful
runs, and the page reads them that way.

Retention: rows older than 90 days (by `at`) are folded into `daily`, one entry per workflow, event
and day: `{"day", "wf", "event", "runs", "failed", "jobs": {name: median seconds}, "longest":
median of the runs' longest job}`; a day already folded is never recomputed, so folding is
idempotent, and `cancelled` days older than that are dropped. Runs older than the edge are not
fetched. `cancelled` is recounted, not added to, for the days a pass read whole (a listing is
newest first, so a pass that read it down to day D saw every later day completely); a first pass
over a file without counts therefore fills only the recent days of the push listings, and
`--full` fills the rest.

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
import statistics
import sys
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
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
WORKFLOWS = {"ci": "ci.yml", "sim": "simulators.yml", "nightly": "nightly.yml"}
# which events each series records: `ci` and `sim` take the pushes to main AND the pull requests that
# run them; the nightly only matters on a pull request (its schedule and dispatch runs are releases).
EVENTS = {"ci": ("push", "pull_request"), "sim": ("push", "pull_request"), "nightly": ("pull_request",)}
TITLE_MAX = 120
PR_SUFFIX = re.compile(r"\s*\(#(\d+)\)\s*$")
SHA_LEN = 9
PER_PAGE = 100
FAILED = ("failure", "timed_out")
KEPT = ("success",) + FAILED
RETENTION_DAYS = 90         # per-run rows older than this are folded into one `daily` entry per workflow, event and day


def load(path: str) -> dict:
    """The existing document, or an empty one for a missing, empty or unreadable-as-JSON-empty file."""
    try:
        text = Path(path).read_text(encoding="utf-8")
    except OSError:
        text = ""
    doc = json.loads(text) if text.strip() else {}
    return {"updated": doc.get("updated"), **{k: list(doc.get(k) or []) for k in WORKFLOWS},
            "cancelled": dict(doc.get("cancelled") or {}), "daily": list(doc.get("daily") or [])}


def day_of(stamp: str) -> str:
    return stamp[:10]


def keep(run: dict, event: str = "push", cutoff: datetime | None = None) -> bool:
    if run.get("conclusion") not in KEPT or run.get("event") != event:
        return False
    return cutoff is None or parse_time(run["created_at"]) >= cutoff


def signature(run: dict) -> tuple:
    """What a re-run changes about a run: its verdict and its attempt number."""
    return (run.get("conclusion"), run.get("run_attempt") or 1)


def wall_seconds(run: dict) -> int | None:
    """Created to last update, for a re-run: what a developer waited across the attempts (None for a first attempt)."""
    if (run.get("run_attempt") or 1) < 2 or not run.get("created_at") or not run.get("updated_at"):
        return None
    return round((parse_time(run["updated_at"]) - parse_time(run["created_at"])).total_seconds())


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


def listing_path(repo: str, workflow_file: str, event: str, page: int) -> str:
    # Pushes are listed for main only; pull requests have no branch filter (their head branch is the
    # PR's own, and `branch=main` would match only a PR whose head branch happens to be named main).
    scope = "branch=main" if event == "push" else f"event={event}"
    return f"repos/{repo}/actions/workflows/{workflow_file}/runs?{scope}&per_page={PER_PAGE}&page={page}"


def new_runs(api, repo: str, workflow_file: str, known: dict, max_pages: int, full: bool = False,
             listed: dict | None = None, event: str = "push", cutoff: datetime | None = None,
             info: dict | None = None) -> list[dict]:
    """Kept runs that are new or changed, newest first, stopping at the first page with none.

    `known` maps a stored run id to its (conclusion, attempt), or to None for a row written before
    those were recorded; such a row is never "changed" (only `full` upgrades it, from `listed`).
    `listed`, when given, is filled with every kept run seen, by id (known ones included).

    `full` keeps paging to the end (or `max_pages`) regardless. The listing is not a stable snapshot:
    the first live backfill saw the same run on two pages and missed 45 others, so a run can be
    repeated here and an incremental pass can leave a gap that only a full pass closes.

    `cutoff` is the retention edge: paging ends at the first page that reaches it, and older runs are
    not kept. `info`, when given, is filled with the cancelled runs seen (`cancelled`), the creation
    time of the oldest run seen (`oldest`) and whether the listing was read to its end (`exhausted`),
    which is what lets the caller count cancelled runs per day only for the days this pass saw whole.
    """
    found = []
    seen = set()
    cancelled, oldest, exhausted = {}, None, False
    for page in range(1, max_pages + 1):
        runs = api(listing_path(repo, workflow_file, event, page)).get("workflow_runs", [])
        kept = [r for r in runs if keep(r, event, cutoff)]
        if listed is not None:
            listed.update((r["id"], r) for r in kept)
        for r in runs:
            if r.get("conclusion") == "cancelled" and r.get("event") == event and (cutoff is None or parse_time(r["created_at"]) >= cutoff):
                cancelled[r["id"]] = r
        times = [parse_time(r["created_at"]) for r in runs if r.get("created_at")]
        if times:
            oldest = min(times + ([oldest] if oldest else []))
        fresh = [r for r in kept if r["id"] not in known
                 or (known[r["id"]] is not None and known[r["id"]] != signature(r))]
        found += [r for r in fresh if r["id"] not in seen]
        seen.update(r["id"] for r in fresh)
        if not runs or (cutoff is not None and times and min(times) < cutoff):
            exhausted = True
            break
        if kept and not fresh and not full:
            break
    if info is not None:
        info.update(cancelled=list(cancelled.values()), oldest=oldest, exhausted=exhausted)
    return found


def pr_number(run: dict) -> int | None:
    """The pull request a pull_request run lists itself under, or None.

    The API fills `pull_requests` only while the pull request is open (measured 2026-10-08: 1 of the 100
    newest runs of ci.yml had it), and never for a fork's run; `PrLookup` covers the rest.
    """
    prs = run.get("pull_requests") or []
    return prs[0].get("number") if prs and isinstance(prs[0], dict) else None


class PrLookup:
    """The pull request of a run from its head branch: one `pulls?head=owner:branch` call per distinct branch.

    A merged or closed pull request is not listed on its runs any more, so the number is asked for by
    head branch (the newest pull request of that branch created before the run). A fork's branch is
    looked up under the fork's owner, which finds the pull request on this repository; a run whose
    branch has no pull request (or whose lookup fails) simply has `pr: null`.
    """

    def __init__(self, api, repo: str):
        self.api, self.repo, self.cache = api, repo, {}

    def __call__(self, run: dict) -> int | None:
        number = pr_number(run)
        if number is not None:
            return number
        owner = ((run.get("head_repository") or {}).get("owner") or {}).get("login")
        branch = run.get("head_branch")
        if not owner or not branch:
            return None
        key = (owner, branch)
        if key not in self.cache:
            try:
                found = self.api(f"repos/{self.repo}/pulls?state=all&head={owner}:{branch}&per_page=5")
            except ApiError:
                found = []
            self.cache[key] = [(p.get("created_at") or "", p["number"]) for p in found if isinstance(p, dict) and "number" in p]
        before = [n for created, n in self.cache[key] if not created or created <= (run.get("created_at") or "")]
        return (before or [n for _, n in self.cache[key]] or [None])[0]


def row_for(api, repo: str, run: dict, lookup=None) -> dict:
    jobs = api(f"repos/{repo}/actions/runs/{run['id']}/jobs?per_page=100").get("jobs", [])
    title, pr = split_title(subject(run))
    row = {"id": run["id"], "at": run.get("run_started_at") or run["created_at"],
           "sha": run["head_sha"][:SHA_LEN], "title": title, "pr": pr}
    if run.get("event") == "pull_request":
        row["pr"] = (lookup or pr_number)(run)
        row["event"] = "pull_request"
    row.update(conclusion=run["conclusion"], attempt=run.get("run_attempt") or 1, jobs=job_seconds(jobs))
    wall = wall_seconds(run)
    if wall is not None:
        row["wall"] = wall
    failed = failed_jobs(jobs)
    if failed:
        row["failed"] = failed
    return row


def row_event(row: dict) -> str:
    return row.get("event") or "push"


def fold(doc: dict, now: str) -> int:
    """Fold per-run rows older than RETENTION_DAYS into `daily`, one entry per workflow, event and day.

    A day already folded is never recomputed (a later pass could only see part of it), so folding is
    idempotent and the entries are written once. Cancelled counts older than the edge are dropped.
    Returns how many rows were folded.
    """
    edge = (parse_time(now) - timedelta(days=RETENTION_DAYS)).strftime("%Y-%m-%d")
    have = {(d["wf"], d["event"], d["day"]) for d in doc["daily"]}
    groups: dict = {}
    folded = 0
    for key in WORKFLOWS:
        keepers = []
        for r in doc[key]:
            if day_of(r["at"]) >= edge:
                keepers.append(r)
                continue
            folded += 1
            groups.setdefault((key, row_event(r), day_of(r["at"])), []).append(r)
        doc[key] = keepers
    for (wf, event, day), rows in groups.items():
        if (wf, event, day) in have:
            continue
        names = sorted({n for r in rows for n in r.get("jobs", {})})
        longest = [max(r["jobs"].values()) for r in rows if r.get("jobs")]
        doc["daily"].append({"day": day, "wf": wf, "event": event, "runs": len(rows),
                             "failed": sum(1 for r in rows if r.get("conclusion") in FAILED),
                             "jobs": {n: round(statistics.median(r["jobs"][n] for r in rows if n in r.get("jobs", {}))) for n in names},
                             "longest": round(statistics.median(longest)) if longest else None})
    doc["daily"].sort(key=lambda d: (d["day"], d["wf"], d["event"]))
    doc["cancelled"] = {d: c for d, c in doc["cancelled"].items() if d >= edge}
    return folded


def count_cancelled(doc: dict, infos: list[dict]) -> None:
    """Per-day counts of cancelled runs, for the days this pass saw whole.

    Every listing is newest first, so a pass that read a listing down to the creation day D saw each
    day after D completely; a listing read to its end saw them all. Those days are recounted from
    the listings (never added to), which makes the pass idempotent without remembering run ids. The
    push listings stop at the first known page, so a file that has no counts yet gets only the
    recent days until a `--full` pass reads them to the end.
    """
    boundary = None            # days at or before it were not seen whole by some listing
    for info in infos:
        if not info["exhausted"] and info["oldest"] is not None:
            day = info["oldest"].strftime("%Y-%m-%d")
            boundary = day if boundary is None else max(boundary, day)
    counts: dict = {}
    for info in infos:
        for r in info["cancelled"]:
            day = day_of(r["created_at"])
            if boundary is None or day > boundary:
                counts.setdefault(day, {"push": 0, "pull_request": 0})[r["event"]] += 1
    old = {d: c for d, c in doc["cancelled"].items() if boundary is not None and d <= boundary}
    doc["cancelled"] = {**old, **counts}


def update(doc: dict, api, repo: str, max_pages: int, workers: int, now: str, full: bool = False) -> tuple[list, list]:
    """Merge new and re-run rows into `doc` in place; returns (added, refreshed) rows."""
    added, refreshed = [], []
    cutoff = parse_time(now) - timedelta(days=RETENTION_DAYS)
    infos = []
    lookup = PrLookup(api, repo)
    for key, wf in WORKFLOWS.items():
        known = {r["id"]: ((r["conclusion"], r.get("attempt") or 1) if r.get("conclusion") else None) for r in doc[key]}
        listed: dict = {}
        fresh = []
        for event in EVENTS[key]:
            info: dict = {}
            fresh += new_runs(api, repo, wf, known, max_pages, full, listed, event, cutoff, info)
            infos.append(info)
        if full:        # the listing carries all of this, so stored rows are refreshed without refetching jobs
            refetched = {f["id"] for f in fresh}
            for r in doc[key]:
                run = listed.get(r["id"])
                if run and r["id"] not in refetched:
                    title, pr = split_title(subject(run))
                    r["title"], r["pr"] = title, (lookup(run) if run.get("event") == "pull_request" else pr)
                    r["conclusion"], r["attempt"] = signature(run)
                    wall = wall_seconds(run)
                    if wall is not None:
                        r["wall"] = wall
        with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
            rows = list(pool.map(lambda r: row_for(api, repo, r, lookup), fresh))
        unique = {r["id"]: r for r in doc[key] + rows}      # also heals duplicates an older file may hold
        doc[key] = sorted(unique.values(), key=lambda r: (r["at"], r["id"]))
        added += [r for r in rows if r["id"] not in known]
        refreshed += [r for r in rows if r["id"] in known]
    before = (json.dumps(doc["cancelled"], sort_keys=True), len(doc["daily"]))
    count_cancelled(doc, infos)
    folded = fold(doc, now)
    changed = before != (json.dumps(doc["cancelled"], sort_keys=True), len(doc["daily"])) or folded
    if added or refreshed or changed or not doc.get("updated"):
        doc["updated"] = now
    return added, refreshed


def dump(doc: dict) -> str:
    """One row per line, so a commit to the data branch diffs as the rows it added."""
    def block(rows):
        return "[\n" + ",\n".join(json.dumps(r, separators=(",", ":")) for r in rows) + "\n]" if rows else "[]"
    cancelled = json.dumps(dict(sorted(doc["cancelled"].items())), separators=(",", ":"))
    return (f'{{"updated":{json.dumps(doc["updated"])},\n"ci":{block(doc["ci"])},\n'
            f'"sim":{block(doc["sim"])},\n"nightly":{block(doc["nightly"])},\n'
            f'"cancelled":{cancelled},\n"daily":{block(doc["daily"])}}}\n')


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
              f"(ci={sum(r in doc['ci'] for r in added)} sim={sum(r in doc['sim'] for r in added)} nightly={sum(r in doc['nightly'] for r in added)}"
              f"{', ' + span[0] + ' .. ' + span[-1] if span else ''}) and re-record {len(refreshed)}")
        return 0
    tmp = f"{args.out}.tmp"
    Path(tmp).write_text(dump(doc), encoding="utf-8")
    os.replace(tmp, args.out)
    print(f"ci-history: +{len(added)} rows, {len(refreshed)} re-recorded; ci={len(doc['ci'])} sim={len(doc['sim'])} nightly={len(doc['nightly'])} daily={len(doc['daily'])}; updated {doc['updated']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
