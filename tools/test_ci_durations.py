#!/usr/bin/env python3
"""tools/ci-durations.py against canned `gh api` JSON: statistics, the growth flag, output shapes."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

spec = importlib.util.spec_from_file_location("durations", Path(__file__).with_name("ci-durations.py"))
d = importlib.util.module_from_spec(spec)
spec.loader.exec_module(d)

T0 = datetime(2026, 9, 1, tzinfo=timezone.utc)


def stamp(dt):
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def job(name, secs, step_secs=None, conclusion="success"):
    started = T0
    out = {"name": name, "conclusion": conclusion, "started_at": stamp(started),
           "completed_at": stamp(started + timedelta(seconds=secs)), "steps": []}
    for step, s in (step_secs or {}).items():
        out["steps"].append({"name": step, "conclusion": "success", "started_at": stamp(started),
                             "completed_at": stamp(started + timedelta(seconds=s))})
    return out


def canned(per_workflow):
    """per_workflow: {file: [jobs-list per run, NEWEST first]} -> an api() double."""
    runs = {}
    jobs = {}
    rid = 1000
    for file, history in per_workflow.items():
        runs[file] = []
        for age, job_list in enumerate(history):
            rid += 1
            runs[file].append({"id": rid, "created_at": stamp(T0 - timedelta(hours=age))})
            jobs[rid] = job_list
    calls = []

    def api(path):
        calls.append(path)
        for file, listing in runs.items():
            if f"/workflows/{file}/runs" in path:
                assert "branch=main" in path and "status=success" in path
                return {"workflow_runs": list(reversed(listing))}   # API order must not matter
        run_id = int(path.split("/runs/")[1].split("/")[0])
        return {"jobs": jobs[run_id]}

    api.calls = calls
    return api


def history(recent_secs, previous_secs, name="build", recent=5):
    return [[job(name, recent_secs, {"compile": recent_secs - 10, "tiny": 2})] for _ in range(recent)] + \
           [[job(name, previous_secs, {"compile": previous_secs - 10, "tiny": 2})] for _ in range(25)]


class Statistics(unittest.TestCase):
    def test_median_and_nearest_rank_p90(self):
        self.assertEqual(d.median([1, 9, 5]), 5)
        self.assertEqual(d.p90(list(range(1, 11))), 9)        # ceil(0.9 * 10) = 9th value
        self.assertEqual(d.p90([4]), 4)
        self.assertIsNone(d.p90([]))

    def test_unfinished_and_failed_items_have_no_duration(self):
        self.assertIsNone(d.seconds(job("x", 5, conclusion="failure")))
        self.assertIsNone(d.seconds({"conclusion": "success", "started_at": None, "completed_at": None}))
        self.assertEqual(d.seconds(job("x", 90)), 90)

    def test_formatting(self):
        self.assertEqual(d.fmt(45), "45s")
        self.assertEqual(d.fmt(125), "2m05s")
        self.assertEqual(d.fmt(None), "n/a")


class GrowthFlag(unittest.TestCase):
    def analyse(self, recent_secs, previous_secs):
        runs = [{"jobs": j} for j in history(recent_secs, previous_secs)]
        return d.analyse(runs, 5, 8)["jobs"][0]

    def test_over_20_percent_and_30_seconds_is_flagged(self):
        j = self.analyse(400, 300)
        self.assertTrue(j["grown"])
        self.assertEqual((j["recent_median_s"], j["previous_median_s"], j["change_s"]), (400, 300, 100))
        self.assertAlmostEqual(j["change_pct"], 33.33, places=1)

    def test_relative_growth_below_the_absolute_bar_is_not_flagged(self):
        self.assertFalse(self.analyse(16, 10)["grown"])      # +60% but only +6 s

    def test_absolute_growth_below_the_relative_bar_is_not_flagged(self):
        self.assertFalse(self.analyse(1000, 970)["grown"])   # +30 s but only +3%

    def test_exactly_at_the_thresholds_is_not_flagged(self):
        self.assertFalse(self.analyse(120, 100)["grown"])    # +20% exactly (and < 30 s)
        self.assertFalse(self.analyse(330, 300)["grown"])    # +30 s exactly, +10%

    def test_a_shrinking_job_is_not_flagged(self):
        self.assertFalse(self.analyse(100, 400)["grown"])

    def test_without_a_previous_window_nothing_is_flagged(self):
        runs = [{"jobs": j} for j in history(400, 300)[:5]]
        j = d.analyse(runs, 5, 8)["jobs"][0]
        self.assertFalse(j["grown"])
        self.assertIsNone(j["change_s"])

    def test_one_slow_outlier_does_not_move_the_median(self):
        hist = history(300, 300)
        hist[0] = [job("build", 3000)]
        j = d.analyse([{"jobs": x} for x in hist], 5, 8)["jobs"][0]
        self.assertFalse(j["grown"])
        self.assertEqual(j["recent_median_s"], 300)

    def test_failed_jobs_are_left_out(self):
        runs = [{"jobs": [job("build", 100)]}, {"jobs": [job("build", 9999, conclusion="failure")]}]
        self.assertEqual(d.analyse(runs, 5, 8)["jobs"][0]["runs"], 1)


class Steps(unittest.TestCase):
    def test_slowest_steps_are_ranked_by_median_and_capped(self):
        runs = [{"jobs": j} for j in history(400, 400)]
        steps = d.analyse(runs, 5, 1)["slowest_steps"]
        self.assertEqual([(s["job"], s["step"], s["median_s"]) for s in steps], [("build", "compile", 390)])


class EndToEnd(unittest.TestCase):
    def run_main(self, api, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = d.main(list(args), api=api)
        return code, out.getvalue(), err.getvalue()

    def api(self):
        return canned({"ci.yml": history(400, 300, name="host unit"),
                       "simulators.yml": history(100, 100, name="linux sim")})

    def test_markdown_flags_only_the_grown_job(self):
        code, out, _ = self.run_main(self.api())
        self.assertEqual(code, 0)
        self.assertIn("### CI (30 successful runs on main; recent = newest 5)", out)
        self.assertIn("### Simulator CI", out)
        row = next(line for line in out.splitlines() if line.startswith("| host unit |"))
        self.assertIn("GROWN", row)
        self.assertIn("+100s (+33%)", row)
        self.assertNotIn("GROWN", next(line for line in out.splitlines() if line.startswith("| linux sim |")))
        self.assertIn("Grown (recent median > +20% and > +30 s): CI: host unit", out)

    def test_json_output_has_the_same_facts(self):
        code, out, _ = self.run_main(self.api(), "--json")
        self.assertEqual(code, 0)
        doc = json.loads(out)
        self.assertEqual(sorted(doc["workflows"]), ["CI", "Simulator CI"])
        job_row = doc["workflows"]["CI"]["jobs"][0]
        self.assertTrue({"job", "runs", "median_s", "p90_s", "recent_median_s", "previous_median_s",
                         "change_s", "change_pct", "grown"} <= set(job_row))
        self.assertTrue(job_row["grown"])
        self.assertEqual(doc["workflows"]["CI"]["runs_used"], 30)

    def test_it_asks_for_main_and_successful_runs_and_one_jobs_call_per_run(self):
        api = self.api()
        self.run_main(api, "--workflow", "CI")
        listing = [c for c in api.calls if c.endswith("per_page=30")]
        self.assertEqual(len(listing), 1)
        self.assertEqual(sum("/jobs?" in c for c in api.calls), 30)

    def test_bad_window_arguments_and_api_failures(self):
        code, _, err = self.run_main(self.api(), "--runs", "5", "--recent", "5")
        self.assertEqual(code, 2)
        self.assertIn("--recent < --runs", err)

        def broken(path):
            raise d.ApiError("boom")

        code, _, err = self.run_main(broken)
        self.assertEqual(code, 2)
        self.assertIn("boom", err)


if __name__ == "__main__":
    unittest.main()
