#!/usr/bin/env python3
"""tools/ci-history.py against a fake `gh api`: incremental merge, dedupe, filtering, paging stop."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import tempfile
import threading
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

spec = importlib.util.spec_from_file_location("history", Path(__file__).with_name("ci-history.py"))
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)

T0 = datetime(2026, 9, 1, tzinfo=timezone.utc)
NOW = "2026-10-02T12:00:00Z"


def stamp(dt):
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def run(rid, hours=0, conclusion="success", event="push", title="Build: a thing (#1)", message=None):
    return {"id": rid, "head_commit": {"message": message} if message else None, "conclusion": conclusion, "event": event, "display_title": title,
            "head_sha": f"{rid:040x}", "created_at": stamp(T0 + timedelta(hours=hours)),
            "run_started_at": stamp(T0 + timedelta(hours=hours, minutes=1))}


def job(name, secs, conclusion="success"):
    return {"name": name, "conclusion": conclusion, "started_at": stamp(T0),
            "completed_at": stamp(T0 + timedelta(seconds=secs))}


class Fake:
    """A gh_api double. pages: {workflow file: [[run, ...] per page, newest first]}; jobs: {run id: [job]}."""

    def __init__(self, pages, jobs=None):
        self.pages, self.jobs, self.calls = pages, jobs or {}, []
        self.lock = threading.Lock()

    def __call__(self, path):
        with self.lock:
            self.calls.append(path)
        for wf, pages in self.pages.items():
            if f"/workflows/{wf}/runs" in path:
                assert "status=" not in path, "status= is stale on the runs listing"
                n = int(path.split("&page=")[1])
                return {"workflow_runs": pages[n - 1] if n <= len(pages) else []}
        if "/jobs" in path:
            rid = int(path.split("/runs/")[1].split("/")[0])
            return {"jobs": self.jobs.get(rid, [job("only job", 10)])}
        raise AssertionError(path)

    def job_calls(self):
        return sorted(int(c.split("/runs/")[1].split("/")[0]) for c in self.calls if "/jobs" in c)

    def list_calls(self, wf):
        return [c for c in self.calls if f"/workflows/{wf}/runs" in c]


def go(fake, src_doc=None, extra=()):
    with tempfile.TemporaryDirectory() as d:
        src, out = Path(d, "in.json"), Path(d, "out.json")
        if src_doc is not None:
            src.write_text(src_doc if isinstance(src_doc, str) else json.dumps(src_doc))
        with contextlib.redirect_stdout(io.StringIO()):
            code = h.main(["--in", str(src), "--out", str(out), *extra], api=fake, now=NOW)
        return code, (json.loads(out.read_text()) if out.exists() else None), (out.read_text() if out.exists() else "")


class HistoryTest(unittest.TestCase):
    def test_backfill_from_nothing_builds_sorted_rows(self):
        fake = Fake({"ci.yml": [[run(3, 3), run(2, 2), run(1, 1)]], "simulators.yml": [[run(9, 5)]]},
                    {3: [job("a", 90), job("b", 30)]})
        code, doc, _ = go(fake)
        self.assertEqual(code, 0)
        self.assertEqual([r["id"] for r in doc["ci"]], [1, 2, 3])
        self.assertEqual([r["id"] for r in doc["sim"]], [9])
        top = doc["ci"][2]
        self.assertEqual(top["jobs"], {"a": 90, "b": 30})
        self.assertEqual(top["sha"], f"{3:040x}"[:9])
        self.assertEqual(top["at"], stamp(T0 + timedelta(hours=3, minutes=1)))   # run_started_at, not created_at
        self.assertEqual(doc["updated"], NOW)

    def test_filters_failure_cancelled_pull_request_and_unfinished(self):
        page = [run(1, 1), run(2, 2, conclusion="failure"), run(3, 3, conclusion="cancelled"),
                run(4, 4, event="pull_request"), run(5, 5, conclusion=None), run(6, 6, event="workflow_dispatch")]
        _, doc, _ = go(Fake({"ci.yml": [page], "simulators.yml": [[]]}))
        self.assertEqual([r["id"] for r in doc["ci"]], [1])

    def test_only_successful_jobs_with_timestamps_count(self):
        bad = {"name": "unfinished", "conclusion": "success", "started_at": stamp(T0), "completed_at": None}
        fake = Fake({"ci.yml": [[run(1)]], "simulators.yml": [[]]},
                    {1: [job("ok", 61), job("failed", 5, "failure"), job("skipped", 0, "skipped"), bad]})
        _, doc, _ = go(fake)
        self.assertEqual(doc["ci"][0]["jobs"], {"ok": 61})

    def test_title_is_cut_to_120_characters(self):
        _, doc, _ = go(Fake({"ci.yml": [[run(1, title="x" * 200)]], "simulators.yml": [[]]}))
        self.assertEqual(len(doc["ci"][0]["title"]), 120)
        self.assertIsNone(doc["ci"][0]["pr"])

    def test_split_title_takes_the_last_pr_suffix_from_the_full_title(self):
        self.assertEqual(h.split_title("Build: x (#329) (#343)"), ("Build: x (#329)", 343))
        self.assertEqual(h.split_title("Build: x (#343)  "), ("Build: x", 343))
        self.assertEqual(h.split_title("No number here"), ("No number here", None))
        self.assertEqual(h.split_title("Mentions (#12) midway"), ("Mentions (#12) midway", None))
        self.assertEqual(h.split_title(""), ("", None))
        self.assertEqual(h.split_title(None), ("", None))

    def test_a_long_title_keeps_its_pr_number(self):
        long = "Build: " + "word " * 40 + "(#457)"
        _, doc, _ = go(Fake({"ci.yml": [[run(1, title=long)]], "simulators.yml": [[]]}))
        row = doc["ci"][0]
        self.assertEqual(row["pr"], 457)
        self.assertEqual(len(row["title"]), 120)
        self.assertNotIn("(#457)", row["title"])

    def test_a_cut_display_title_is_replaced_by_the_commit_subject(self):
        cut = "Build: the app crate is an rlib; the ARM archive is asked for by the …"
        full = "Build: the app crate is an rlib; the ARM archive is asked for by the lib (#321)"
        self.assertEqual(h.subject(run(1, title=cut, message=full + "\n\nbody (#999)")), full)
        self.assertEqual(h.subject(run(1, title=cut)), cut)
        _, doc, _ = go(Fake({"ci.yml": [[run(1, title=cut, message=full)]], "simulators.yml": [[]]}))
        self.assertEqual(doc["ci"][0]["pr"], 321)
        self.assertNotIn("…", doc["ci"][0]["title"])

    def test_full_refreshes_title_and_pr_of_stored_rows_without_refetching_jobs(self):
        old = {"id": 1, "at": stamp(T0), "sha": "a" * 9, "title": "Build: cut ...", "jobs": {"keep": 1}}
        pages = [[run(2, 2, title="Build: new (#9)"), run(1, 1, title="Build: whole title (#7)")]]
        fake = Fake({"ci.yml": pages, "simulators.yml": [[]]})
        _, doc, _ = go(fake, {"updated": "u", "sim": [], "ci": [old]}, extra=("--full",))
        self.assertEqual(fake.job_calls(), [2])
        first = doc["ci"][0]
        self.assertEqual((first["title"], first["pr"], first["jobs"]), ("Build: whole title", 7, {"keep": 1}))
        self.assertEqual(doc["ci"][1]["pr"], 9)

    def test_incremental_pass_leaves_stored_titles_alone(self):
        old = {"id": 1, "at": stamp(T0), "sha": "a" * 9, "title": "old", "jobs": {}}
        fake = Fake({"ci.yml": [[run(1, 1, title="Build: whole title (#7)")]], "simulators.yml": [[]]})
        _, doc, _ = go(fake, {"updated": "u", "sim": [], "ci": [old]})
        self.assertEqual(doc["ci"][0], old)

    def test_incremental_fetches_jobs_only_for_new_runs_and_dedupes(self):
        existing = {"updated": "2026-10-01T00:00:00Z", "sim": [],
                    "ci": [{"id": 1, "at": stamp(T0), "sha": "a" * 9, "title": "old", "jobs": {"keep": 1}}]}
        fake = Fake({"ci.yml": [[run(2, 2), run(1, 1)]], "simulators.yml": [[]]})
        _, doc, _ = go(fake, existing)
        self.assertEqual(fake.job_calls(), [2])
        self.assertEqual([r["id"] for r in doc["ci"]], [1, 2])
        self.assertEqual(doc["ci"][0]["jobs"], {"keep": 1})      # the stored row is not refetched or rewritten
        self.assertEqual(doc["updated"], NOW)

    def test_second_run_is_byte_identical_and_keeps_updated(self):
        fake = Fake({"ci.yml": [[run(2, 2), run(1, 1)]], "simulators.yml": [[run(7, 1)]]})
        _, _, first = go(fake)
        later = Fake(fake.pages)
        with tempfile.TemporaryDirectory() as d:
            src, out = Path(d, "in.json"), Path(d, "out.json")
            src.write_text(first)
            with contextlib.redirect_stdout(io.StringIO()):
                h.main(["--in", str(src), "--out", str(out)], api=later, now="2027-01-01T00:00:00Z")
            self.assertEqual(out.read_text(), first)
        self.assertEqual(later.job_calls(), [])

    def test_paging_stops_at_first_page_of_known_runs(self):
        existing = {"updated": "u", "sim": [], "ci": [
            {"id": i, "at": stamp(T0 + timedelta(hours=i)), "sha": "s", "title": "t", "jobs": {}} for i in (1, 2, 3, 4)]}
        pages = [[run(6, 6), run(5, 5)], [run(4, 4), run(3, 3)], [run(2, 2)], [run(1, 1)]]
        fake = Fake({"ci.yml": pages, "simulators.yml": [[]]})
        _, doc, _ = go(fake, existing)
        self.assertEqual(len(fake.list_calls("ci.yml")), 2)       # page 1 had news, page 2 was all known: stop
        self.assertEqual([r["id"] for r in doc["ci"]], [1, 2, 3, 4, 5, 6])

    def test_page_of_only_failures_does_not_stop_paging(self):
        pages = [[run(5, 5, conclusion="failure"), run(4, 4, conclusion="failure")], [run(3, 3)], [run(2, 2)]]
        fake = Fake({"ci.yml": pages, "simulators.yml": [[]]})
        _, doc, _ = go(fake)
        self.assertEqual([r["id"] for r in doc["ci"]], [2, 3])
        self.assertEqual(len(fake.list_calls("ci.yml")), 4)       # three pages, then the empty one ends it

    def test_a_run_repeated_across_pages_is_kept_once(self):
        fake = Fake({"ci.yml": [[run(3, 3), run(2, 2)], [run(2, 2), run(1, 1)]], "simulators.yml": [[]]})
        _, doc, _ = go(fake)
        self.assertEqual([r["id"] for r in doc["ci"]], [1, 2, 3])
        self.assertEqual(fake.job_calls(), [1, 2, 3])

    def test_duplicates_already_in_the_file_are_healed(self):
        row = {"id": 1, "at": stamp(T0), "sha": "s", "title": "t", "jobs": {}}
        _, doc, _ = go(Fake({"ci.yml": [[run(1, 1)]], "simulators.yml": [[]]}), {"updated": "u", "ci": [row, row], "sim": []})
        self.assertEqual([r["id"] for r in doc["ci"]], [1])

    def test_full_pages_past_a_known_page_to_close_a_gap(self):
        existing = {"updated": "u", "sim": [], "ci": [
            {"id": i, "at": stamp(T0 + timedelta(hours=i)), "sha": "s", "title": "t", "jobs": {}} for i in (3, 4)]}
        pages = [[run(4, 4), run(3, 3)], [run(2, 2), run(1, 1)]]
        _, plain, _ = go(Fake({"ci.yml": pages, "simulators.yml": [[]]}), existing)
        self.assertEqual([r["id"] for r in plain["ci"]], [3, 4])
        _, full, _ = go(Fake({"ci.yml": pages, "simulators.yml": [[]]}), existing, extra=("--full",))
        self.assertEqual([r["id"] for r in full["ci"]], [1, 2, 3, 4])

    def test_max_pages_bounds_the_backfill(self):
        pages = [[run(i, i)] for i in range(10, 0, -1)]
        fake = Fake({"ci.yml": pages, "simulators.yml": [[]]})
        _, doc, _ = go(fake, extra=("--max-pages", "3"))
        self.assertEqual(len(fake.list_calls("ci.yml")), 3)
        self.assertEqual(len(doc["ci"]), 3)

    def test_empty_input_forms_all_start_fresh(self):
        for src in (None, "", "   \n"):
            _, doc, _ = go(Fake({"ci.yml": [[run(1)]], "simulators.yml": [[]]}), src)
            self.assertEqual(len(doc["ci"]), 1, repr(src))

    def test_api_failure_exits_2_and_writes_nothing(self):
        def broken(path):
            raise h.ApiError("boom")
        with tempfile.TemporaryDirectory() as d:
            out = Path(d, "out.json")
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                code = h.main(["--in", str(Path(d, "none.json")), "--out", str(out)], api=broken, now=NOW)
            self.assertEqual(code, 2)
            self.assertFalse(out.exists())
            self.assertIn("boom", err.getvalue())

    def test_output_is_one_row_per_line(self):
        _, _, text = go(Fake({"ci.yml": [[run(2, 2), run(1, 1)]], "simulators.yml": [[]]}))
        self.assertEqual(sum(1 for line in text.splitlines() if line.startswith('{"id"')), 2)


if __name__ == "__main__":
    unittest.main()
