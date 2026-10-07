#!/usr/bin/env python3
"""tools/ci-summary.py on synthetic series: step up and down, a noisy flat week, missing days, the
re-run wall rule, push / pull-request separation, the benchmark file and a deterministic document."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

spec = importlib.util.spec_from_file_location("summary", Path(__file__).with_name("ci-summary.py"))
s = importlib.util.module_from_spec(spec)
spec.loader.exec_module(s)

NOW = datetime(2026, 10, 7, 12, 0, tzinfo=timezone.utc)
_ids = iter(range(1, 10 ** 6))


def stamp(dt):
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def row(days_ago, jobs, wf_event="push", conclusion="success", attempt=1, wall=None, sha=None, pr=None, hour=0):
    n = next(_ids)
    r = {"id": n, "at": stamp(NOW - timedelta(days=days_ago, hours=-hour)), "sha": sha or f"{n:09x}", "title": f"change {n}", "pr": pr,
         "conclusion": conclusion, "attempt": attempt, "jobs": jobs}
    if wf_event == "pull_request":
        r["event"] = "pull_request"
    if wall is not None:
        r["wall"] = wall
    return r


def series(history_key, values_by_day, name="job", **kw):
    """one row per entry of {days ago: seconds} (a list = several rows that day)"""
    out = []
    for day, vals in values_by_day.items():
        for i, v in enumerate(vals if isinstance(vals, list) else [vals]):
            out.append(row(day, {name: v}, hour=i, **kw))
    return {history_key: out}


def metric(doc, mid):
    return next(m for m in doc["metrics"] if m["id"] == mid)


def weeks(prev, now_, per_day=2):
    """prev for days 8..14 ago, now_ for days 0..6 ago, `per_day` runs a day"""
    days = {d: [prev] * per_day for d in range(8, 15)}
    days.update({d: [now_] * per_day for d in range(0, 7)})
    return days


class WindowTest(unittest.TestCase):
    def test_a_step_up_is_worse_and_names_the_day_it_started_and_the_commits_in_it(self):
        days = {d: [300] * 2 for d in range(5, 15)}
        days.update({d: [500] * 2 for d in range(0, 5)})       # slower from 4 days ago on
        doc = s.summarize(series("ci", days, name="build"), None, NOW)
        m = metric(doc, "ci|build")
        self.assertEqual((m["state"], m["now"], m["prev7"]), ("worse", 500, 300))
        self.assertEqual(m["change7_pct"], 66.7)
        self.assertEqual({r["id"] for r in doc["regressions"]}, {"ci|build", "push_wait"})      # the job and the wait it sets
        reg = next(r for r in doc["regressions"] if r["id"] == "ci|build")
        self.assertEqual((reg["id"], reg["since"], reg["from"], reg["to"]), ("ci|build", (NOW - timedelta(days=4)).strftime("%Y-%m-%d"), 300, 500))
        self.assertLessEqual(len(reg["commits"]), 10)
        self.assertTrue(reg["commits"] and all(set(c) == {"sha", "title", "pr"} for c in reg["commits"]))
        self.assertEqual(doc["slowest"][0]["id"], "ci|build")

    def test_a_step_down_is_better_and_no_regression_is_reported(self):
        doc = s.summarize(series("ci", weeks(500, 300)), None, NOW)
        self.assertEqual(metric(doc, "ci|job")["state"], "better")
        self.assertEqual(doc["regressions"], [])

    def test_a_noisy_flat_week_stays_flat(self):
        jitter = [0.94, 1.06, 1.0, 0.97, 1.05, 1.02, 0.96]
        days = {d: [round(300 * jitter[d % 7]), round(300 * jitter[(d + 3) % 7])] for d in list(range(8, 15)) + list(range(0, 7))}
        m = metric(s.summarize(series("ci", days), None, NOW), "ci|job")
        self.assertEqual(m["state"], "flat")
        self.assertLess(abs(m["change7_pct"]), 10)

    def test_both_floors_must_be_crossed(self):
        # +50% of a 10 s job is 5 s: above the percent floor, below the 20 s one
        small = metric(s.summarize(series("ci", weeks(10, 15)), None, NOW), "ci|job")
        self.assertEqual((small["change7_pct"], small["state"]), (50.0, "flat"))
        # +30 s of 600 s is 5%: above the absolute floor, below the percent one
        slow = metric(s.summarize(series("ci", weeks(600, 630)), None, NOW), "ci|job")
        self.assertEqual(slow["state"], "flat")
        big = metric(s.summarize(series("ci", weeks(100, 130)), None, NOW), "ci|job")      # 30% and 30 s
        self.assertEqual(big["state"], "worse")

    def test_too_few_samples_before_the_window_is_new_with_no_change(self):
        days = {d: [300] * 2 for d in range(0, 7)}
        days[10] = [300, 300]            # only two samples in the week before
        m = metric(s.summarize(series("ci", days), None, NOW), "ci|job")
        self.assertEqual((m["state"], m["prev7"], m["change7_pct"], m["n_prev"]), ("new", None, None, 2))

    def test_a_quiet_series_falls_back_to_its_last_seven_samples(self):
        # one run every 5 days: only 2 fall in the last 7 days, so `now` is the median of the last 7 samples
        days = {d: [100 + d] for d in range(0, 40, 5)}
        m = metric(s.summarize(series("ci", days), None, NOW), "ci|job")
        self.assertEqual(m["n_now"], 7)
        self.assertEqual(m["now"], 115)       # runs 0,5,...,30 days ago -> 100,105,...,130: the median is 115
        self.assertEqual(m["prev7"], None)

    def test_the_30_day_window_is_the_week_ending_30_days_ago(self):
        days = {d: [300, 300] for d in range(0, 40)}
        days.update({d: [400, 400] for d in range(31, 38)})
        m = metric(s.summarize(series("ci", days), None, NOW), "ci|job")
        self.assertEqual((m["d30"], m["change30_pct"]), (400, -25.0))

    def test_a_series_that_has_stopped_is_not_now(self):
        days = {d: [300] * 2 for d in range(20, 40)}
        self.assertEqual(s.summarize(series("ci", days), None, NOW)["metrics"], [])

    def test_missing_days_leave_no_zero(self):
        days = {d: [300, 300] for d in (0, 1, 2, 8, 12)}
        m = metric(s.summarize(series("ci", days), None, NOW), "ci|job")
        self.assertEqual(m["now"], 300)


class SeriesTest(unittest.TestCase):
    def test_pull_request_and_push_waits_are_separate_series(self):
        hist = {"ci": [row(d, {"a": 100}, sha=f"p{d}", pr=7, wf_event="pull_request") for d in range(0, 6)]
                + [row(d, {"a": 200}, sha=f"m{d}") for d in range(0, 6)]}
        doc = s.summarize(hist, None, NOW)
        self.assertEqual((metric(doc, "pr_wait")["now"], metric(doc, "push_wait")["now"]), (100, 200))
        self.assertEqual(metric(doc, "ci|a")["now"], 200)          # job series are push runs only

    def test_a_commit_waits_for_its_slowest_workflow_and_a_red_run_leaves_it_out(self):
        hist = {"ci": [row(d, {"a": 100}, sha=f"c{d}") for d in range(0, 6)],
                "sim": [row(d, {"b": 400}, sha=f"c{d}") for d in range(0, 6)]}
        hist["sim"][0]["conclusion"] = "failure"            # c0 had a red run
        doc = s.summarize(hist, None, NOW)
        m = metric(doc, "push_wait")
        self.assertEqual((m["now"], m["n_now"]), (400, 5))

    def test_a_rerun_counts_its_wall_time_not_its_longest_job(self):
        hist = {"ci": [row(d, {"a": 100}, sha=f"c{d}", attempt=2, wall=1800) for d in range(0, 6)]}
        self.assertEqual(metric(s.summarize(hist, None, NOW), "push_wait")["now"], 1800)
        self.assertEqual(s.row_seconds({"attempt": 2, "jobs": {"a": 5}}), 5)       # no wall recorded: the jobs

    def test_the_nightly_belongs_to_pull_requests_and_to_the_pull_request_wait(self):
        hist = {"ci": [row(d, {"a": 100}, sha=f"c{d}", wf_event="pull_request") for d in range(0, 6)],
                "nightly": [row(d, {"ARM build": 900}, sha=f"c{d}", wf_event="pull_request") for d in range(0, 6)]}
        doc = s.summarize(hist, None, NOW)
        self.assertEqual((metric(doc, "pr_wait")["now"], metric(doc, "nightly|ARM build")["now"]), (900, 900))

    def test_runner_time_adds_both_workflows_of_a_push(self):
        hist = {"ci": [row(d, {"a": 100, "b": 50}, sha=f"c{d}") for d in range(0, 6)],
                "sim": [row(d, {"c": 300}, sha=f"c{d}") for d in range(0, 6)]}
        self.assertEqual(metric(s.summarize(hist, None, NOW), "minutes")["now"], 450)

    def test_the_benchmark_rows_are_local_metrics_labelled_by_the_record(self):
        recs = [{"date": (NOW - timedelta(days=d)).strftime("%Y-%m-%d"), "at": stamp(NOW - timedelta(days=d)), "commit": f"c{d}",
                 "rows": {"leaf": {"label": "plx_base", "median": 40.0 + (30 if d < 4 else 0)}, "noop": {"label": "No-op build", "median": 3.0}}}
                for d in range(0, 16)]
        doc = s.summarize({}, {"records": recs}, NOW)
        leaf = metric(doc, "bench.leaf")
        self.assertEqual((leaf["group"], leaf["label"], leaf["state"], leaf["now"]), ("local", "plx_base", "worse", 70.0))
        self.assertEqual(metric(doc, "bench.noop")["state"], "flat")
        self.assertEqual(doc["regressions"][0]["id"], "bench.leaf")
        self.assertEqual(doc["slowest"][0]["id"], "bench.leaf")

    def test_a_benchmark_row_with_one_record_is_new(self):
        recs = [{"date": "2026-10-07", "at": "2026-10-07T04:00:00Z", "commit": "abc", "rows": {"tests": {"label": "Unit suite", "median": 30.0}}}]
        m = metric(s.summarize({}, {"records": recs}, NOW), "bench.tests")
        self.assertEqual((m["state"], m["n_now"]), ("new", 1))


class DocumentTest(unittest.TestCase):
    def test_the_document_is_deterministic_and_worse_ones_come_first(self):
        hist = series("ci", weeks(300, 500), name="slow")
        hist["ci"] += series("ci", weeks(300, 300), name="same")["ci"] + series("ci", weeks(300, 200), name="fast")["ci"]
        a, b = s.summarize(hist, None, NOW), s.summarize(json.loads(json.dumps(hist)), None, NOW)
        self.assertEqual(json.dumps(a, sort_keys=True), json.dumps(b, sort_keys=True))
        self.assertEqual([m["state"] for m in a["metrics"] if m["id"].startswith("ci|")], ["worse", "better", "flat"])
        self.assertEqual(a["schema"], 1)
        self.assertEqual(set(a["thresholds"]), set(s.THRESHOLDS))

    def test_main_writes_the_file_and_a_second_run_writes_the_same_bytes(self):
        with tempfile.TemporaryDirectory() as d:
            src, bench, out = Path(d, "h.json"), Path(d, "b.json"), Path(d, "o.json")
            src.write_text(json.dumps(series("ci", weeks(300, 500))))
            bench.write_text(json.dumps({"records": []}))
            args = ["--history", str(src), "--bench", str(bench), "--out", str(out), "--now", stamp(NOW)]
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(s.main(args), 0)
                first = out.read_text()
                self.assertEqual(s.main(args), 0)
                self.assertEqual(out.read_text(), first)
                self.assertEqual(s.main(["--history", str(Path(d, "missing.json")), "--out", str(out)]), 2)
            self.assertEqual(json.loads(first)["metrics"][0]["state"], "worse")


if __name__ == "__main__":
    unittest.main()
