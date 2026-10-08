#!/usr/bin/env python3
"""tools/ci-history.py against a fake `gh api`: incremental merge, dedupe, filtering, paging stop."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import re
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


def run(rid, hours=0, conclusion="success", event="push", title="Build: a thing (#1)", message=None, attempt=1, pr=None, took=None, branch="feature", owner="GLinnik21"):
    return {"id": rid, "head_commit": {"message": message} if message else None, "conclusion": conclusion, "event": event, "display_title": title,
            "run_attempt": attempt, "head_sha": f"{rid:040x}", "created_at": stamp(T0 + timedelta(hours=hours)),
            "run_started_at": stamp(T0 + timedelta(hours=hours, minutes=1)),
            "updated_at": stamp(T0 + timedelta(hours=hours, seconds=took if took is not None else 600)),
            "pull_requests": [{"number": pr}] if pr else [], "head_branch": branch,
            "head_repository": {"owner": {"login": owner}}}


def job(name, secs, conclusion="success"):
    return {"name": name, "conclusion": conclusion, "started_at": stamp(T0),
            "completed_at": stamp(T0 + timedelta(seconds=secs))}


class Fake:
    """A gh_api double. pages: {workflow file: [[run, ...] per page, newest first]} answers the push listing
    (`branch=main`), {"pr:" + workflow file: ...} the pull-request listing (`event=pull_request`); a listing
    that is not given is empty. jobs: {run id: [job]}."""

    def __init__(self, pages, jobs=None):
        self.pages, self.jobs, self.calls, self.pulls = pages, jobs or {}, [], {}
        self.lock = threading.Lock()

    def __call__(self, path):
        with self.lock:
            self.calls.append(path)
        if "/pulls?" in path:
            return self.pulls.get(re.search(r"head=([^&]+)", path).group(1), [])
        m = re.search(r"/workflows/([^/]+)/runs\?(branch=main|event=pull_request)&", path)
        if m:
            assert "status=" not in path, "status= is stale on the runs listing"
            wf, scope = m.groups()
            pages = self.pages.get(wf if scope == "branch=main" else "pr:" + wf, [])
            n = int(path.split("&page=")[1])
            return {"workflow_runs": pages[n - 1] if n <= len(pages) else []}
        if "/jobs" in path:
            rid = int(path.split("/runs/")[1].split("/")[0])
            return {"jobs": self.jobs.get(rid, [job("only job", 10)])}
        raise AssertionError(path)

    def job_calls(self):
        return sorted(int(c.split("/runs/")[1].split("/")[0]) for c in self.calls if "/jobs" in c)

    def list_calls(self, wf):
        return [c for c in self.calls if f"/workflows/{wf}/runs?branch=main" in c]

    def pr_list_calls(self, wf):
        return [c for c in self.calls if f"/workflows/{wf}/runs?event=pull_request" in c]


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

    def test_keeps_finished_push_runs_and_drops_cancelled_pull_request_and_unfinished(self):
        page = [run(1, 1), run(2, 2, conclusion="failure"), run(3, 3, conclusion="cancelled"),
                run(4, 4, event="pull_request"), run(5, 5, conclusion=None), run(6, 6, event="workflow_dispatch"),
                run(7, 7, conclusion="timed_out"), run(8, 8, conclusion="skipped")]
        _, doc, _ = go(Fake({"ci.yml": [page], "simulators.yml": [[]]}))
        self.assertEqual([(r["id"], r["conclusion"]) for r in doc["ci"]],
                         [(1, "success"), (2, "failure"), (7, "timed_out")])

    def test_a_failed_run_keeps_only_its_successful_jobs_and_names_the_failed_ones(self):
        fake = Fake({"ci.yml": [[run(1, conclusion="failure", attempt=2), run(2)]], "simulators.yml": [[]]},
                    {1: [job("ok", 61), job("broke", 5, "failure"), job("hung", 600, "timed_out"), job("skipped", 0, "skipped")]})
        _, doc, text = go(fake)
        bad, good = doc["ci"]
        self.assertEqual((bad["conclusion"], bad["attempt"], bad["jobs"], bad["failed"]), ("failure", 2, {"ok": 61}, ["broke", "hung"]))
        self.assertEqual((good["conclusion"], good["attempt"]), ("success", 1))
        self.assertNotIn("failed", good)
        self.assertEqual(sum(1 for line in text.splitlines() if line.startswith('{"id"')), 2)

    def test_a_job_that_appears_and_one_that_disappears_are_recorded_as_they_were_not_filled_in(self):
        fake = Fake({"ci.yml": [[run(3, 3), run(2, 2), run(1, 1)]], "simulators.yml": [[]]},
                    {1: [job("a", 10), job("old", 20)], 2: [job("a", 11), job("old", 21), job("new", 5)],
                     3: [job("a", 12), job("new", 6)]})
        _, doc, _ = go(fake)
        self.assertEqual([r["jobs"] for r in doc["ci"]],
                         [{"a": 10, "old": 20}, {"a": 11, "old": 21, "new": 5}, {"a": 12, "new": 6}])

    def test_a_rerun_after_a_failure_is_recorded_again_with_the_attempt_that_is_now_the_runs(self):
        old = {"id": 1, "at": stamp(T0), "sha": "a" * 9, "title": "t", "pr": None, "conclusion": "failure", "attempt": 1,
               "jobs": {"ok": 1}, "failed": ["flaky"]}
        fake = Fake({"ci.yml": [[run(1, 1, attempt=2)]], "simulators.yml": [[]]}, {1: [job("ok", 61), job("flaky", 30)]})
        _, doc, _ = go(fake, {"updated": "u", "sim": [], "ci": [old]})
        self.assertEqual(fake.job_calls(), [1])
        row = doc["ci"][0]
        self.assertEqual((row["conclusion"], row["attempt"], row["jobs"]), ("success", 2, {"ok": 61, "flaky": 30}))
        self.assertNotIn("failed", row)
        self.assertEqual(len(doc["ci"]), 1)

    def test_a_stored_run_that_did_not_change_is_not_refetched(self):
        old = {"id": 1, "at": stamp(T0), "sha": "a" * 9, "title": "t", "pr": None, "conclusion": "failure", "attempt": 1, "jobs": {}}
        fake = Fake({"ci.yml": [[run(1, 1, conclusion="failure")]], "simulators.yml": [[]]})
        go(fake, {"updated": "u", "sim": [], "ci": [old]})
        self.assertEqual(fake.job_calls(), [])

    def test_a_row_from_before_conclusion_existed_is_left_alone_until_a_full_pass(self):
        legacy = {"id": 1, "at": stamp(T0), "sha": "a" * 9, "title": "t", "pr": None, "jobs": {"keep": 1}}
        listing = [[run(1, 1, attempt=3)]]
        fake = Fake({"ci.yml": listing, "simulators.yml": [[]]})
        _, plain, _ = go(fake, {"updated": "u", "sim": [], "ci": [legacy]})
        self.assertEqual((plain["ci"][0], fake.job_calls()), (legacy, []))
        fake = Fake({"ci.yml": listing, "simulators.yml": [[]]})
        _, full, _ = go(fake, {"updated": "u", "sim": [], "ci": [legacy]}, extra=("--full",))
        row = full["ci"][0]
        self.assertEqual((row["conclusion"], row["attempt"], row["jobs"], fake.job_calls()), ("success", 3, {"keep": 1}, []))

    def test_dry_run_counts_and_writes_nothing(self):
        fake = Fake({"ci.yml": [[run(3, 3), run(2, 2, conclusion="failure"), run(1, 1)]], "simulators.yml": [[run(9, 5)]]})
        with tempfile.TemporaryDirectory() as d:
            src, out = Path(d, "in.json"), Path(d, "out.json")
            src.write_text(json.dumps({"updated": "u", "sim": [], "ci": [{"id": 1, "at": stamp(T0), "sha": "s", "title": "t", "jobs": {}}]}))
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                code = h.main(["--in", str(src), "--dry-run", "--out", str(out)], api=fake, now=NOW)
            self.assertEqual(code, 0)
            self.assertFalse(out.exists())
            self.assertIn("would add 3 rows (ci=2 sim=1", buf.getvalue())
            self.assertIn("nothing written", buf.getvalue())
            self.assertEqual(json.loads(src.read_text())["sim"], [])
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            h.main(["--in", "x"], api=fake, now=NOW)       # neither --out nor --dry-run

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
        self.assertEqual((first["conclusion"], first["attempt"]), ("success", 1))
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
                h.main(["--in", str(src), "--out", str(out)], api=later, now="2026-10-20T00:00:00Z")
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

    def test_page_of_only_dropped_runs_does_not_stop_paging(self):
        pages = [[run(5, 5, conclusion="cancelled"), run(4, 4, event="pull_request")], [run(3, 3)], [run(2, 2)]]
        fake = Fake({"ci.yml": pages, "simulators.yml": [[]]})
        _, doc, _ = go(fake)
        self.assertEqual([r["id"] for r in doc["ci"]], [2, 3])
        self.assertEqual(len(fake.list_calls("ci.yml")), 4)       # three pages, then the empty one ends it

    def test_a_failure_older_than_the_first_known_page_needs_a_full_pass(self):
        # Rows from before failures were recorded are all successes, so a failure among them is "new" to an
        # incremental pass only while its page also holds something new; a --full pass finds every one.
        known = [{"id": i, "at": stamp(T0 + timedelta(hours=i)), "sha": "s", "title": "t", "jobs": {}} for i in (2, 3)]
        pages = [[run(3, 3), run(2, 2)], [run(1, 1, conclusion="failure")]]
        _, plain, _ = go(Fake({"ci.yml": pages, "simulators.yml": [[]]}), {"updated": "u", "sim": [], "ci": known})
        self.assertEqual([r["id"] for r in plain["ci"]], [2, 3])
        _, full, _ = go(Fake({"ci.yml": pages, "simulators.yml": [[]]}), {"updated": "u", "sim": [], "ci": known}, extra=("--full",))
        self.assertEqual([(r["id"], r.get("conclusion")) for r in full["ci"]], [(1, "failure"), (2, "success"), (3, "success")])

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


    # ---- pull requests, the nightly, re-runs, cancelled runs, retention ----

    def test_a_pull_request_run_is_stored_with_its_event_and_its_pr_number_and_a_push_row_is_unchanged(self):
        fake = Fake({"ci.yml": [[run(1, 1, title="Build: merged (#7)")]],
                     "pr:ci.yml": [[run(5, 5, event="pull_request", title="wip: try a thing", pr=42)]]})
        _, doc, text = go(fake)
        push, pr = sorted(doc["ci"], key=lambda r: r["id"])
        self.assertEqual(list(push), ["id", "at", "sha", "title", "pr", "conclusion", "attempt", "jobs"])   # byte-compatible keys
        self.assertEqual((push["pr"], "event" in push), (7, False))
        self.assertEqual((pr["event"], pr["pr"], pr["sha"], pr["title"]), ("pull_request", 42, f"{5:040x}"[:9], "wip: try a thing"))
        self.assertTrue(all("branch=main" not in c for c in fake.calls if "event=pull_request" in c))
        self.assertEqual(sum(1 for line in text.splitlines() if line.startswith('{"id"')), 2)

    def test_a_merged_pull_requests_run_finds_its_number_by_head_branch_once_per_branch(self):
        # the API lists a run's pull_requests only while the PR is open
        fake = Fake({"pr:ci.yml": [[run(5, 5, event="pull_request", branch="feat"), run(4, 4, event="pull_request", branch="feat"),
                                    run(3, 3, event="pull_request", branch="orphan"), run(2, 2, event="pull_request", pr=9, branch="open")]]})
        fake.pulls["GLinnik21:feat"] = [{"number": 77, "created_at": "2026-08-30T00:00:00Z"}, {"number": 12, "created_at": "2026-07-01T00:00:00Z"}]
        _, doc, _ = go(fake)
        self.assertEqual({r["id"]: r["pr"] for r in doc["ci"]}, {5: 77, 4: 77, 3: None, 2: 9})
        asked = [c for c in fake.calls if "/pulls?" in c]
        self.assertEqual(sorted(c.split("head=")[1].split("&")[0] for c in asked), ["GLinnik21:feat", "GLinnik21:orphan"])

    def test_a_fork_pull_requests_run_is_looked_up_under_the_forks_owner(self):
        fake = Fake({"pr:ci.yml": [[run(5, 5, event="pull_request", branch="patch-1", owner="someone")]]})
        fake.pulls["someone:patch-1"] = [{"number": 31, "created_at": "2026-08-01T00:00:00Z"}]
        _, doc, _ = go(fake)
        self.assertEqual(doc["ci"][0]["pr"], 31)

    def test_the_nightly_is_recorded_for_pull_requests_only(self):
        fake = Fake({"nightly.yml": [[run(1, 1, event="schedule"), run(2, 2, event="push")]],
                     "pr:nightly.yml": [[run(3, 3, event="pull_request", pr=9), run(4, 4, event="schedule")]]},
                    {3: [job("ARM build + gates", 900)]})
        _, doc, _ = go(fake)
        self.assertEqual([(r["id"], r["event"], r["pr"], r["jobs"]) for r in doc["nightly"]], [(3, "pull_request", 9, {"ARM build + gates": 900})])
        self.assertEqual(fake.list_calls("nightly.yml"), [])     # no push listing for it at all

    def test_a_rerun_records_the_wall_time_across_its_attempts(self):
        _, doc, _ = go(Fake({"ci.yml": [[run(2, 2, attempt=2, took=5400), run(1, 1)]]}))
        first, again = doc["ci"]
        self.assertEqual((first["id"], "wall" in first), (1, False))
        self.assertEqual((again["attempt"], again["wall"]), (2, 5400))

    def test_the_pull_request_listing_stops_at_its_first_known_page_too(self):
        known = [{"id": i, "event": "pull_request", "pr": 3, "at": stamp(T0 + timedelta(hours=i)), "sha": "s", "title": "t", "jobs": {},
                  "conclusion": "success", "attempt": 1} for i in (3, 4)]
        pages = [[run(5, 5, event="pull_request"), run(4, 4, event="pull_request")], [run(3, 3, event="pull_request")], [run(2, 2, event="pull_request")]]
        fake = Fake({"pr:ci.yml": pages})
        _, doc, _ = go(fake, {"updated": "u", "ci": known, "sim": []})
        self.assertEqual(len(fake.pr_list_calls("ci.yml")), 2)
        self.assertEqual([r["id"] for r in doc["ci"]], [3, 4, 5])

    def test_cancelled_runs_are_counted_per_day_and_not_stored_and_recounting_changes_nothing(self):
        page = [run(1, 1, conclusion="cancelled"), run(2, 2, conclusion="cancelled"), run(3, 3), run(4, 30, conclusion="cancelled")]
        pr_page = [run(5, 5, event="pull_request", conclusion="cancelled"), run(6, 6, event="pull_request")]
        pages = {"ci.yml": [page], "simulators.yml": [[run(7, 7, conclusion="cancelled")]], "pr:ci.yml": [pr_page]}
        _, doc, first = go(Fake(pages))
        self.assertEqual(doc["cancelled"], {"2026-09-01": {"push": 3, "pull_request": 1}, "2026-09-02": {"push": 1, "pull_request": 0}})
        self.assertEqual(sorted(r["id"] for r in doc["ci"]), [3, 6])
        _, again, second = go(Fake(pages), first)
        self.assertEqual(again["cancelled"], doc["cancelled"])
        self.assertEqual(second, first)

    def test_a_day_a_pass_read_only_in_part_keeps_its_earlier_count(self):
        # page 1 holds news, page 2 is all known: the pass read the listing down to day 2 only
        stored = {"updated": "u", "sim": [], "cancelled": {"2026-09-01": {"push": 4, "pull_request": 0}, "2026-09-02": {"push": 9, "pull_request": 0}},
                  "ci": [{"id": 3, "at": stamp(T0 + timedelta(hours=30)), "sha": "s", "title": "t", "jobs": {}, "conclusion": "success", "attempt": 1}]}
        pages = [[run(4, 50, conclusion="cancelled"), run(5, 49)], [run(3, 30), run(1, 2, conclusion="cancelled")]]
        _, doc, _ = go(Fake({"ci.yml": pages}), stored)
        self.assertEqual(doc["cancelled"]["2026-09-01"], {"push": 4, "pull_request": 0})      # not seen whole: unchanged
        self.assertEqual(doc["cancelled"]["2026-09-03"], {"push": 1, "pull_request": 0})      # seen whole: recounted

    def test_rows_past_retention_fold_into_one_daily_entry_and_folding_is_idempotent(self):
        def row(i, day, jobs, conclusion="success", **extra):
            return {"id": i, "at": f"{day}T10:00:00Z", "sha": "s", "title": "t", "pr": None, "conclusion": conclusion, "attempt": 1, "jobs": jobs, **extra}
        old = [row(1, "2026-06-01", {"a": 100, "b": 40}), row(2, "2026-06-01", {"a": 140}), row(3, "2026-06-01", {"a": 5}, "failure"),
               row(4, "2026-06-01", {"a": 60}, event="pull_request", pr=8), row(5, "2026-06-02", {"a": 70})]
        fresh = row(9, "2026-09-30", {"a": 1})
        src = {"updated": "u", "ci": old + [fresh], "sim": [], "nightly": [], "cancelled": {"2026-06-01": {"push": 2, "pull_request": 0}, "2026-09-30": {"push": 1, "pull_request": 0}}}
        _, doc, first = go(Fake({}), src)
        self.assertEqual([r["id"] for r in doc["ci"]], [9])
        by = {(d["day"], d["event"]): d for d in doc["daily"]}
        self.assertEqual(by[("2026-06-01", "push")], {"day": "2026-06-01", "wf": "ci", "event": "push", "runs": 3, "failed": 1,
                                                       "jobs": {"a": 100, "b": 40}, "longest": 100})
        self.assertEqual((by[("2026-06-01", "pull_request")]["runs"], by[("2026-06-02", "push")]["jobs"]), (1, {"a": 70}))
        self.assertNotIn("2026-06-01", doc["cancelled"])
        _, _, second = go(Fake({}), first)
        self.assertEqual(second, first)

    def test_a_day_already_folded_is_not_recomputed_from_partial_data(self):
        done = {"day": "2026-06-01", "wf": "ci", "event": "push", "runs": 50, "failed": 2, "jobs": {"a": 111}, "longest": 111}
        late = {"id": 1, "at": "2026-06-01T10:00:00Z", "sha": "s", "title": "t", "pr": None, "conclusion": "success", "attempt": 1, "jobs": {"a": 1}}
        _, doc, _ = go(Fake({}), {"updated": "u", "ci": [late], "sim": [], "daily": [done]})
        self.assertEqual(doc["daily"], [done])
        self.assertEqual(doc["ci"], [])

    def test_runs_past_retention_are_not_fetched_and_end_the_paging(self):
        fake = Fake({"ci.yml": [[run(3, 3), run(2, 2)], [run(1, hours=-24 * 100)], [run(10, 1)]]})
        _, doc, _ = go(fake)
        self.assertEqual([r["id"] for r in doc["ci"]], [2, 3])
        self.assertEqual(fake.job_calls(), [2, 3])
        self.assertEqual(len(fake.list_calls("ci.yml")), 2)


SITE_CI = Path(__file__).resolve().parents[1] / "site" / "ci"


class PageTest(unittest.TestCase):
    def test_the_page_and_its_data_are_ascii_for_the_site_font_subset(self):
        (SITE_CI / "index.html").read_bytes().decode("ascii")

    def test_the_page_names_no_job_it_would_then_have_to_keep_current(self):
        page = (SITE_CI / "index.html").read_text(encoding="ascii")
        script = page[page.index("<script>"):]
        for name in ("host lint", "cross-build", "macOS simulator", "Linux simulator", "host unit tests", "host harness", "armv7"):
            self.assertNotIn(name, script)

    def test_the_page_reads_conclusions_attempts_events_and_the_three_data_files(self):
        script = (SITE_CI / "index.html").read_text(encoding="ascii")
        for needle in ("r.conclusion", "attempt", "pull_request", "r.wall", "ci-history.json", "ci-summary.json", "build-history.json"):
            self.assertIn(needle, script)

    def test_the_local_build_charts_judge_noise_per_row_and_show_failures_and_the_runner(self):
        page = (SITE_CI / "index.html").read_text(encoding="ascii")
        script = page[page.index("<script>"):]
        # per row (the record's own `noisy`), never the day-level flag the first record carried
        self.assertIn("rec.rows[k].noisy", script)
        self.assertNotIn("r.noisy", script)
        self.assertNotIn("reported load", page)
        # a failed step is named on its day, with the recorder's reason
        for needle in ("failure_notes", "failed on the runner", "BENCH_FAIL_WHAT"):
            self.assertIn(needle, script)
        # the runner is described from the record next to the statement that the numbers come from one
        self.assertIn("benchRunnerText", script)
        self.assertIn('id="bench-runner"', page)
        # a lone day still shows its range
        self.assertIn("a single day has no neighbour", script)

    def test_the_hand_entered_milestones_are_gone_from_the_page_and_the_tree(self):
        self.assertNotIn("milestone", (SITE_CI / "index.html").read_text(encoding="ascii").lower())
        self.assertFalse((SITE_CI / "milestones.json").exists())

    def test_the_page_loads_nothing_from_another_origin(self):
        page = (SITE_CI / "index.html").read_text(encoding="ascii")
        self.assertNotRegex(page, r"<script[^>]+src=")
        for host in set(re.findall(r"https?://([^/\s\"'`)<>$]+)", page)):
            self.assertIn(host, ("raw.githubusercontent.com", "github.com", "plxnative.com"))


if __name__ == "__main__":
    unittest.main()
