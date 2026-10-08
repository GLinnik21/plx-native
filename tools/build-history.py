#!/usr/bin/env python3
"""Turn `tools/build-bench.py --json` documents into ONE record per day of build-history.json.

The daily job (.github/workflows/build-bench.yml) measures the tip of main on a GitHub macOS runner,
and this is the step that files the result on the `ci-metrics` branch, beside ci-history.json:

    python3 tools/build-history.py --bench bench.json [--bench check.json ...] \\
        --in data/build-history.json --out data/build-history.json [--date YYYY-MM-DD] [--run-id N] [--commit-env]

The file is {"updated": ISO, "records": [...]} with one record per line, oldest first:

    {"date": "2026-10-08", "at": ISO, "commit": "<12 hex>",
     "runner": {"image": "macos15 20260101.1", "os": "macOS", "arch": "ARM64", "cores": 3, "cpu": "Apple M1 (Virtual)"},
     "toolchain": "rustc 1.99.0-nightly (...)", "runs": 3, "warnings": 18, "failed": [],
     "failure_notes": {}, "workflow_run": 123456789,
     "rows": {"noop": {"label": "No-op build", "title": "No-op host test build",
                       "median": 2.4, "min": 2.3, "max": 2.6, "runs": 3, "spread": 0.125, "noisy": false}, ...}}

Row ids are build-bench's scenario ids (`noop`, `leaf`, `screens`, `tests`, ...) plus `check`,
`check.cargo` and `check.python` for the whole gate and its two branches. Seconds throughout.

What it guarantees, each pinned by ci/test_build_history.py:

* one record per day: a re-run on the same date REPLACES that day's record, every other day is kept;
* a benchmark that failed writes NOTHING and exits 1 (a failure, a target it could not restore, or a
  requested scenario with no number). The exceptions are the OPTIONAL scenarios, `check` (the whole
  host gate) and `tests` (the unit suite): one of them being red on the runner is listed in `failed`,
  with a one-line reason in `failure_notes`, and the rest of the day still records, so the page can say
  "make check failed on the runner on <date>" instead of silently having no point;
* noise is judged PER ROW, from the row's own runs: `spread` is (max - min) / median, and a row is
  `noisy` when its spread is above NOISY_SPREAD and the runs differ by at least NOISY_MIN_RANGE seconds
  (a 0.9 s spread on a 0.3 s no-op build is not noise worth a mark). A row of one run has no spread. The
  load-average and swap warnings the benchmark prints are kept as the raw count `warnings` and decide
  nothing: on a 3-core runner running a parallel build the load always exceeds the core count. The
  rule is applied again to every stored row each time the file is written, so retuning the constants
  retunes the history, and a record written before the rule existed gains the fields;
* nothing identifying: the runner block is the CPU brand string, core count, OS name, architecture
  and the image name GitHub documents, copied from the benchmark's own `runner` block, and a failure
  note has any home-directory path reduced to `~`.

Exit status: 0 recorded; 1 the benchmark failed, nothing written; 2 unusable input (a missing or
unreadable bench file, a data file that is not ours, no rows at all).
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

# Scenarios whose failure must not cost the day's other numbers: the whole-gate run depends on the
# runner having every tool `make check` shells out to, and the unit suite is red when any one test is.
OPTIONAL_IDS = frozenset({"check", "tests"})
# When a row's runs disagree so much that its median is a guess. Provisional: chosen from the first
# record (see docs/agent-reference.md), where the runs of a row differed by 26% to 57% of the median
# because the first round of each row ran 40-55% slower than the other two. The raw `spread` is
# recorded, and the rule is applied again to every stored row on each write, so this is one number to
# retune once there are weeks of rows, not a verdict frozen into the history.
NOISY_SPREAD = 0.5
NOISY_MIN_RANGE = 2.0   # seconds: below this a large ratio is timer granularity, not noise
NOTE_MAX = 300
LABELS = {
    "noop": "No-op build",
    "tests": "Unit suite",
    "arm": "ARM staticlib",
    "check": "make check",
    "check.cargo": "make check: cargo branch",
    "check.python": "make check: python branch",
}
EDIT_TITLE = re.compile(r"^Edit (leaf|hub) \((\S+)")


class Failed(Exception):
    """The benchmark did not produce a trustworthy result: write nothing, exit 1."""


class Unusable(Exception):
    """The input is not ours: exit 2."""


def row_label(sid: str, title: str) -> str:
    """A short name for a legend: the crate for an edit row, the scenario's own word otherwise."""
    if sid in LABELS:
        return LABELS[sid]
    m = EDIT_TITLE.match(title)
    if not m:
        return title
    crate = "app crate" if m.group(2) == "app" else m.group(2)
    label = f"{crate} hub" if m.group(1) == "hub" else crate
    return label + (" (incremental)" if sid.endswith("-inc") else "")


def scrub(text: str) -> str:
    """A failure note is public: a home directory in it becomes `~`, and it is short."""
    return re.sub(r"/(Users|home)/[^/\s]+", r"/\1/~", str(text)).replace("\n", " ")[:NOTE_MAX]


def is_optional_only(doc: dict) -> bool:
    ids = {sc.get("id") for sc in doc.get("scenarios", [])}
    return bool(ids) and ids <= OPTIONAL_IDS


def classify(doc: dict) -> tuple[list[str], dict[str, str]]:
    """(why the document cannot be trusted, {optional scenario id: why it is red}); both empty when it is clean.

    A scenario of OPTIONAL_IDS that is red costs only its own rows. Anything else being red (a
    build that failed, a scenario that never finished, an edit target that was not put back) costs
    the day. A document that holds nothing but optional scenarios and carries a run-level failure is
    that scenario's failure (the `check` document, whose single scenario is the whole gate)."""
    hard: list[str] = []
    soft: dict[str, str] = {}
    scenarios = doc.get("scenarios", [])
    if doc.get("failure"):
        if is_optional_only(doc):
            for sc in scenarios:
                soft[sc.get("id")] = scrub(doc["failure"])
        else:
            hard.append(scrub(doc["failure"])[:200])
    if doc.get("restored_clean") is False:
        hard.append("an edit target was not restored clean")
    for sc in scenarios:
        if sc.get("status") in ("failed", "incomplete"):
            if sc.get("id") in OPTIONAL_IDS:
                soft.setdefault(sc.get("id"), scrub(sc.get("note") or f"scenario {sc.get('id')} is {sc['status']}"))
            else:
                hard.append(f"scenario {sc.get('id')} is {sc['status']}")
    return hard, soft


def annotate(row: dict) -> dict:
    """Give a row its `spread` and `noisy` (see the module docstring), from its median, min, max and runs."""
    med, lo, hi, runs = row.get("median"), row.get("min"), row.get("max"), row.get("runs") or 0
    if not all(isinstance(v, (int, float)) for v in (med, lo, hi)):
        return row
    spread = round((hi - lo) / med, 3) if med > 0 and runs >= 2 else 0.0
    row["spread"] = spread
    row["noisy"] = bool(runs >= 2 and spread > NOISY_SPREAD and hi - lo >= NOISY_MIN_RANGE)
    return row


def rows_of(doc: dict) -> dict[str, dict]:
    rows: dict[str, dict] = {}
    for sc in doc.get("scenarios", []):
        sid = sc.get("id")
        if sc.get("status") != "ok" or "median" not in sc or sid == "sizes":
            continue
        rows[sid] = annotate({"label": row_label(sid, sc.get("name", "")), "title": sc.get("name", sid),
                              "median": sc["median"], "min": sc["min"], "max": sc["max"], "runs": len(sc["samples"])})
        for branch, st in (sc.get("branches") or {}).items():
            if "median" in st:
                bid = f"{sid}.{branch}"
                rows[bid] = annotate({"label": row_label(bid, ""), "title": f"{sc.get('name', sid)}, {branch} branch",
                                      "median": st["median"], "min": st["min"], "max": st["max"], "runs": len(sc["samples"])})
    return rows


def runner_of(doc: dict) -> dict:
    r = doc.get("runner") or {}
    host = doc.get("host") or {}
    image = " ".join(x for x in (r.get("image_os"), r.get("image_version")) if x)
    return {"image": image, "os": r.get("runner_os") or r.get("system") or host.get("os", ""),
            "arch": r.get("runner_arch") or r.get("arch", ""), "cores": r.get("cores") or host.get("cores"),
            "cpu": r.get("cpu") or host.get("machine", "")}


def build_record(docs: list[dict], date: str | None = None, run_id: int | None = None,
                 commit: str | None = None, now: datetime | None = None) -> dict:
    """The day's record from the benchmark documents, or Failed / Unusable."""
    if not docs:
        raise Unusable("no benchmark document given")
    now = now or datetime.now(timezone.utc)
    rows: dict[str, dict] = {}
    notes: dict[str, str] = {}
    primary = []
    for doc in docs:
        hard, soft = classify(doc)
        if hard:
            raise Failed("the benchmark failed, nothing recorded: " + "; ".join(hard))
        notes.update(soft)
        primary.append(doc)
        rows.update(rows_of(doc))
    if not rows:
        raise Unusable("the benchmark documents hold no timed scenario")
    shas = {d.get("git_sha") for d in primary if d.get("git_sha")}
    if len(shas) > 1:
        raise Unusable("the benchmark documents were taken at different commits: " + ", ".join(sorted(shas)))
    sha = commit or (shas.pop() if shas else "")
    if not re.fullmatch(r"[0-9a-f]{7,40}", sha or ""):
        raise Unusable(f"no usable commit id ({sha!r})")
    warnings = sum(len(d.get("warnings") or []) for d in docs)
    ref = next((d for d in primary if not is_optional_only(d)), primary[0])
    return {"date": date or now.strftime("%Y-%m-%d"), "at": now.strftime("%Y-%m-%dT%H:%M:%SZ"), "commit": sha[:12],
            "runner": runner_of(ref), "toolchain": ref.get("toolchain", ""), "runs": ref.get("runs"),
            "warnings": warnings, "failed": sorted(notes), "failure_notes": dict(sorted(notes.items())),
            "workflow_run": run_id, "rows": rows}


def merge(existing: dict | None, record: dict, now: datetime | None = None) -> dict:
    """The data file with `record` filed under its date (replacing that day's record, keeping the rest)."""
    now = now or datetime.now(timezone.utc)
    records = [r for r in (existing or {}).get("records", []) if r.get("date") != record["date"]]
    records.append(record)
    for rec in records:
        rec.pop("noisy", None)   # a day was once called noisy as a whole; noise is a property of a row
        for row in (rec.get("rows") or {}).values():
            annotate(row)
    records.sort(key=lambda r: r["date"])
    return {"updated": now.strftime("%Y-%m-%dT%H:%M:%SZ"), "records": records}


def dump(doc: dict) -> str:
    """One record per line, so a day's change is one line of a diff and the file stays greppable."""
    lines = ",\n".join(json.dumps(r, separators=(",", ":")) for r in doc["records"])
    return '{"updated":%s,\n"records":[\n%s\n]}\n' % (json.dumps(doc["updated"]), lines)


def load_existing(path: Path) -> dict | None:
    if not path.exists():
        return None
    try:
        doc = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        raise Unusable(f"{path} is not readable JSON ({e}); not overwriting it")
    if not isinstance(doc, dict) or not isinstance(doc.get("records"), list):
        raise Unusable(f"{path} has no `records` list; not overwriting it")
    return doc


def write_atomic(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=".build-history.")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
        os.replace(tmp, path)
    except BaseException:
        if os.path.exists(tmp):
            os.unlink(tmp)
        raise


def main(argv=None, now: datetime | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--bench", action="append", required=True, metavar="JSON", help="a build-bench --json document (repeatable)")
    p.add_argument("--in", dest="inp", metavar="JSON", help="the data file to extend (absent = start a new one)")
    p.add_argument("--out", required=True, metavar="JSON", help="where to write the merged data file")
    p.add_argument("--date", help="the record's date, YYYY-MM-DD (default: today, UTC)")
    p.add_argument("--run-id", type=int, help="the workflow run id, recorded as workflow_run")
    p.add_argument("--commit-env", action="store_true", help="take the commit from $GITHUB_SHA instead of the benchmark's own")
    args = p.parse_args(argv)
    try:
        if args.date and not re.fullmatch(r"\d{4}-\d{2}-\d{2}", args.date):
            raise Unusable(f"--date {args.date!r} is not YYYY-MM-DD")
        docs = []
        for path in args.bench:
            try:
                docs.append(json.loads(Path(path).read_text(encoding="utf-8")))
            except (OSError, ValueError) as e:
                raise Unusable(f"cannot read {path}: {e}")
        existing = load_existing(Path(args.inp)) if args.inp else None
        commit = os.environ.get("GITHUB_SHA", "")[:12] if args.commit_env else None
        if args.commit_env and not commit:
            raise Unusable("--commit-env given but GITHUB_SHA is not set")
        record = build_record(docs, date=args.date, run_id=args.run_id, commit=commit, now=now)
        write_atomic(Path(args.out), dump(merge(existing, record, now)))
    except Failed as e:
        print(f"build-history: {e}", file=sys.stderr)
        return 1
    except Unusable as e:
        print(f"build-history: {e}", file=sys.stderr)
        return 2
    print(f"build-history: recorded {record['date']} at {record['commit']}: {len(record['rows'])} rows"
          + (f", noisy rows: {', '.join(noisy)}" if (noisy := [k for k, v in record["rows"].items() if v.get("noisy")]) else "")
          + (f", failed: {', '.join(record['failed'])}" if record["failed"] else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
