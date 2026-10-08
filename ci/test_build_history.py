#!/usr/bin/env python3
"""tools/build-history.py: the daily build-time record (build-history.json on the ci-metrics branch).

Hermetic: synthetic `build-bench --json` documents, files in a temp dir, a fixed clock. Pins what the
daily job relies on: one record per day (a same-day re-run replaces that day's), a failed benchmark
writes nothing, a red or unrunnable `check` (or a red unit suite) costs only its own rows and is named in
the record, noise is judged per row from the row's own runs (the load warnings are only counted), and
nothing identifying reaches the file.
"""
from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

import check_steps

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "tools" / "build-history.py"
spec = importlib.util.spec_from_file_location("build_history", SCRIPT)
bh = importlib.util.module_from_spec(spec)
sys.modules["build_history"] = bh
spec.loader.exec_module(bh)

NOW = datetime(2026, 10, 8, 4, 30, 0, tzinfo=timezone.utc)
RUNNER = {"cpu": "Apple M1 (Virtual)", "cores": 3, "system": "Darwin 24.6.0", "arch": "arm64",
          "runner_os": "macOS", "runner_arch": "ARM64", "image_os": "macos15", "image_version": "20260101.1"}
ALLOWED_RUNNER_KEYS = {"image", "os", "arch", "cores", "cpu"}


def scenario(sid, name, median, runs=3, status="ok", **extra):
    sc = {"id": sid, "name": name, "status": status, "note": "", "info": "",
          "samples": [{"seconds": median}] * runs if status == "ok" else []}
    if status == "ok":
        sc.update(median=median, min=round(median * 0.9, 2), max=round(median * 1.2, 2))
    sc.update(extra)
    return sc


def main_doc(sha="0123456789ab", warnings=(), **over):
    doc = {"schema": 1, "git_sha": sha, "tree_dirty": False, "toolchain": "rustc 1.99.0-nightly (fake 2026-01-01)",
           "host": {"os": "Darwin 24.6.0", "machine": "Apple M1 (Virtual)", "cores": 3}, "runner": dict(RUNNER),
           "runs": 3, "warnings": list(warnings), "failure": None, "restored_clean": True,
           "scenarios": [scenario("noop", "No-op host test build", 2.4),
                         scenario("leaf", "Edit leaf (plx_base cbuf.rs), non-incremental", 61.0),
                         scenario("screens", "Edit leaf (plx_screens clock_readout.rs), non-incremental", 48.0),
                         scenario("app", "Edit leaf (app coldstart.rs), non-incremental", 30.0),
                         scenario("hub", "Edit hub (plx_ui lib.rs), non-incremental", 52.0),
                         scenario("tests", "Unit suite run (default features)", 40.0),
                         {"id": "sizes", "name": "Target dir sizes", "status": "ok", "samples": []}]}
    doc.update(over)
    return doc


def check_doc(status="ok", **over):
    sc = (scenario("check", "make check, whole host gate", 300.0, runs=1,
                   branches={"cargo": {"median": 250.0, "min": 250.0, "max": 250.0},
                             "python": {"median": 110.0, "min": 110.0, "max": 110.0}})
          if status == "ok" else scenario("check", "make check, whole host gate", 0, status=status))
    doc = main_doc(runs=1, scenarios=[sc], failure=None if status == "ok" else "check: the gate exited 2")
    doc.update(over)
    return doc


class Sandbox:
    def __enter__(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self._tmp.name)
        return self

    def __exit__(self, *exc):
        self._tmp.cleanup()

    def write(self, name, doc):
        path = self.dir / name
        path.write_text(json.dumps(doc), encoding="utf-8")
        return str(path)

    def run(self, *args, env=None):
        full = {k: v for k, v in os.environ.items() if k != "GITHUB_SHA"}
        full.update(env or {})
        return subprocess.run([sys.executable, str(SCRIPT), *args], capture_output=True, text=True, env=full)


class RecordTests(unittest.TestCase):
    def test_the_record_shape(self):
        rec = bh.build_record([main_doc(), check_doc()], date="2026-10-08", run_id=77, now=NOW)
        self.assertEqual(rec["date"], "2026-10-08")
        self.assertEqual(rec["at"], "2026-10-08T04:30:00Z")
        self.assertEqual(rec["commit"], "0123456789ab")
        self.assertEqual(rec["runs"], 3)
        self.assertEqual((rec["warnings"], rec["failed"], rec["failure_notes"], rec["workflow_run"]), (0, [], {}, 77))
        self.assertNotIn("noisy", rec)   # noise is a property of a row, not of a day
        self.assertEqual(list(rec["rows"]), ["noop", "leaf", "screens", "app", "hub", "tests", "check", "check.cargo", "check.python"])
        self.assertEqual({k: v["label"] for k, v in rec["rows"].items()},
                         {"noop": "No-op build", "leaf": "plx_base", "screens": "plx_screens", "app": "app crate",
                          "hub": "plx_ui hub", "tests": "Unit suite", "check": "make check",
                          "check.cargo": "make check: cargo branch", "check.python": "make check: python branch"})
        leaf = rec["rows"]["leaf"]
        self.assertEqual((leaf["median"], leaf["min"], leaf["max"], leaf["runs"]), (61.0, 54.9, 73.2, 3))
        self.assertEqual((leaf["spread"], leaf["noisy"]), (0.3, False))
        self.assertEqual(leaf["title"], "Edit leaf (plx_base cbuf.rs), non-incremental")
        self.assertEqual(rec["rows"]["check"]["runs"], 1)
        self.assertEqual(rec["rows"]["check.cargo"]["median"], 250.0)
        self.assertNotIn("sizes", rec["rows"])
        self.assertEqual(rec["toolchain"], "rustc 1.99.0-nightly (fake 2026-01-01)")

    def test_the_runner_names_the_machine_and_nothing_that_identifies_it(self):
        rec = bh.build_record([main_doc()], date="2026-10-08", now=NOW)
        self.assertEqual(rec["runner"], {"image": "macos15 20260101.1", "os": "macOS", "arch": "ARM64",
                                         "cores": 3, "cpu": "Apple M1 (Virtual)"})
        # a document from a developer's own machine has no runner variables: the system fields stand in
        doc = main_doc()
        doc["runner"] = {"cpu": "Apple M4", "cores": 10, "system": "Darwin 25.0.0", "arch": "arm64"}
        local = bh.build_record([doc], date="2026-10-08", now=NOW)["runner"]
        self.assertEqual(set(local), ALLOWED_RUNNER_KEYS)
        self.assertEqual((local["image"], local["os"], local["arch"]), ("", "Darwin 25.0.0", "arm64"))
        # an unexpected field in the document does not travel into the record
        doc["runner"]["hostname"] = "somebodys-mac.local"
        self.assertNotIn("somebodys-mac", json.dumps(bh.build_record([doc], date="2026-10-08", now=NOW)))

    def test_edit_labels_come_from_the_titles(self):
        self.assertEqual(bh.row_label("ui", "Edit leaf (plx_ui dwell.rs), non-incremental"), "plx_ui")
        self.assertEqual(bh.row_label("leaf-inc", "Edit leaf (plx_base cbuf.rs), incremental"), "plx_base (incremental)")
        self.assertEqual(bh.row_label("hub-inc", "Edit hub (plx_ui lib.rs), incremental"), "plx_ui hub (incremental)")
        self.assertEqual(bh.row_label("odd", "Something else"), "Something else")

    def test_load_warnings_are_counted_and_decide_nothing(self):
        # On a 3-core runner running a parallel build the load always exceeds the core count (the first
        # real record carried 18 such warnings), so a day that "was loaded" says nothing about a row.
        warnings = [f"{sid} run {n}: load 33.00 > 3 cores" for n in (1, 2, 3) for sid in ("noop", "leaf", "app")]
        rec = bh.build_record([main_doc(warnings=warnings), check_doc()], date="2026-10-08", now=NOW)
        self.assertEqual(rec["warnings"], 9)
        self.assertNotIn("noisy", rec)
        self.assertFalse([k for k, row in rec["rows"].items() if row["noisy"]])

    def test_a_row_is_noisy_when_its_own_runs_disagree(self):
        def row(median, lo, hi, runs=3):
            return bh.annotate({"median": median, "min": lo, "max": hi, "runs": runs})
        self.assertEqual(bh.NOISY_SPREAD, 0.5)
        self.assertEqual((row(40.0, 20.0, 60.0)["spread"], row(40.0, 20.0, 60.0)["noisy"]), (1.0, True))
        self.assertFalse(row(40.0, 30.0, 50.0)["noisy"])                 # 50% exactly is not above it
        self.assertTrue(row(40.0, 30.0, 50.5)["noisy"])
        self.assertFalse(row(0.33, 0.30, 1.21)["noisy"], "0.9 s on a no-op build is timer granularity")
        self.assertGreater(row(0.33, 0.30, 1.21)["spread"], 2.0)          # ...though the ratio is huge, and recorded
        one = row(116.0, 116.0, 116.0, runs=1)
        self.assertEqual((one["spread"], one["noisy"]), (0.0, False))     # one run has no spread
        self.assertEqual(bh.annotate({"label": "no numbers"}), {"label": "no numbers"})

    def test_the_first_real_record_classifies_as_documented(self):
        # run 37694891086 on a 3-core M1 runner: every row's first round ran 40-55% slower than the others
        def timed(sid, name, med, lo, hi):
            return scenario(sid, name, med, min=lo, max=hi)
        doc = main_doc(warnings=["w"] * 18, scenarios=[
            timed("noop", "No-op host test build", 0.33, 0.3, 1.21),
            timed("leaf", "Edit leaf (plx_base cbuf.rs), non-incremental", 115.75, 99.7, 139.9),
            timed("ui", "Edit leaf (plx_ui dwell.rs), non-incremental", 46.97, 46.4, 65.88),
            timed("screens", "Edit leaf (plx_screens clock_readout.rs), non-incremental", 37.76, 32.52, 52.45),
            timed("app", "Edit leaf (app coldstart.rs), non-incremental", 18.06, 17.77, 28.0),
            timed("tests", "Unit suite run (default features)", 53.8, 48.5, 62.61)])
        rows = bh.build_record([doc], date="2026-10-07", now=NOW)["rows"]
        self.assertEqual({k: v["spread"] for k, v in rows.items()},
                         {"noop": 2.758, "leaf": 0.347, "ui": 0.415, "screens": 0.528, "app": 0.566, "tests": 0.262})
        self.assertEqual(sorted(k for k, v in rows.items() if v["noisy"]), ["app", "screens"])

    def test_the_commit_can_be_given_and_documents_must_agree(self):
        rec = bh.build_record([main_doc()], commit="feedfacefeedface0000", date="2026-10-08", now=NOW)
        self.assertEqual(rec["commit"], "feedfacefeed")
        with self.assertRaises(bh.Unusable):
            bh.build_record([main_doc(), main_doc(sha="aaaaaaaaaaaa")], date="2026-10-08", now=NOW)
        with self.assertRaises(bh.Unusable):
            bh.build_record([main_doc(sha="")], date="2026-10-08", now=NOW)


class FailureTests(unittest.TestCase):
    def test_a_failed_benchmark_records_nothing(self):
        for broken in (main_doc(failure="leaf edit-rebuild: cargo exited 101"),
                       main_doc(restored_clean=False),
                       main_doc(scenarios=[scenario("noop", "No-op host test build", 2.4),
                                           scenario("leaf", "Edit leaf", 0, status="incomplete")])):
            with self.assertRaises(bh.Failed):
                bh.build_record([broken, check_doc()], date="2026-10-08", now=NOW)

    def test_a_red_or_unrunnable_check_costs_only_its_own_rows(self):
        for status in ("failed", "incomplete"):
            rec = bh.build_record([main_doc(), check_doc(status=status)], date="2026-10-08", now=NOW)
            self.assertEqual(rec["failed"], ["check"])
            self.assertIn("leaf", rec["rows"])
            self.assertFalse([k for k in rec["rows"] if k.startswith("check")])

    def test_the_record_says_why_the_gate_was_red_and_nothing_private(self):
        doc = check_doc(status="failed", failure="check: the gate exited 2: 2 step(s) failed: python3 ci/test_check_elf.py; "
                                                  "python3 tools/test_check_parallel.py | see /Users/somebody/work/x\nand more " + "x" * 400)
        rec = bh.build_record([main_doc(), doc], date="2026-10-08", now=NOW)
        note = rec["failure_notes"]["check"]
        self.assertTrue(note.startswith("check: the gate exited 2: 2 step(s) failed: python3 ci/test_check_elf.py"))
        self.assertNotIn("somebody", note)
        self.assertIn("/Users/~", note)
        self.assertNotIn("\n", note)
        self.assertLessEqual(len(note), bh.NOTE_MAX)

    def test_a_red_unit_suite_costs_only_its_own_row(self):
        red = scenario("tests", "Unit suite run (default features)", 40.0, status="failed",
                       note="3 unit test(s) failed: a::b, c::d, e::f")
        red.update(median=40.0, min=36.0, max=48.0)   # a red suite still has times; they are not kept
        doc = main_doc(scenarios=[s for s in main_doc()["scenarios"] if s["id"] != "tests"] + [red])
        rec = bh.build_record([doc, check_doc()], date="2026-10-08", now=NOW)
        self.assertEqual(rec["failed"], ["tests"])
        self.assertEqual(rec["failure_notes"], {"tests": "3 unit test(s) failed: a::b, c::d, e::f"})
        self.assertNotIn("tests", rec["rows"])
        self.assertIn("leaf", rec["rows"])
        self.assertIn("check", rec["rows"])

    def test_a_run_level_failure_in_a_document_with_build_rows_still_costs_the_day(self):
        # the exemption is for scenarios that are optional, not for whatever else was in the document
        with self.assertRaises(bh.Failed):
            bh.build_record([main_doc(failure="tests: unit suite: no `test result:` line")], date="2026-10-08", now=NOW)

    def test_a_check_document_alone_is_not_a_day(self):
        with self.assertRaises(bh.Unusable):
            bh.build_record([check_doc(status="failed")], date="2026-10-08", now=NOW)
        with self.assertRaises(bh.Unusable):
            bh.build_record([], date="2026-10-08", now=NOW)


class MergeTests(unittest.TestCase):
    def test_every_stored_row_is_judged_again_on_each_write(self):
        legacy = {"date": "2026-10-07", "at": "2026-10-07T22:44:43Z", "commit": "e494cea2fbec", "runs": 3, "warnings": 18,
                  "noisy": True, "failed": ["check"], "rows": {
                      "screens": {"label": "plx_screens", "median": 37.76, "min": 32.52, "max": 52.45, "runs": 3},
                      "leaf": {"label": "plx_base", "median": 115.75, "min": 99.7, "max": 139.9, "runs": 3}}}
        doc = bh.merge({"updated": "x", "records": [legacy]}, self.record("2026-10-08"), NOW)
        old = doc["records"][0]
        self.assertNotIn("noisy", old)    # the day-level flag of the first record is gone
        self.assertEqual(old["warnings"], 18)     # ...the raw count stays
        self.assertEqual((old["rows"]["screens"]["spread"], old["rows"]["screens"]["noisy"]), (0.528, True))
        self.assertEqual((old["rows"]["leaf"]["spread"], old["rows"]["leaf"]["noisy"]), (0.347, False))

    def record(self, date, leaf=61.0):
        doc = main_doc()
        doc["scenarios"][1] = scenario("leaf", "Edit leaf (plx_base cbuf.rs), non-incremental", leaf)
        return bh.build_record([doc], date=date, now=NOW)

    def test_a_same_day_rerun_replaces_that_days_record_and_nothing_else(self):
        doc = None
        for d in ("2026-10-07", "2026-10-06", "2026-10-08"):
            doc = bh.merge(doc, self.record(d), NOW)
        self.assertEqual([r["date"] for r in doc["records"]], ["2026-10-06", "2026-10-07", "2026-10-08"])
        again = bh.merge(doc, self.record("2026-10-07", leaf=99.0), NOW)
        self.assertEqual([r["date"] for r in again["records"]], ["2026-10-06", "2026-10-07", "2026-10-08"])
        self.assertEqual([r["rows"]["leaf"]["median"] for r in again["records"]], [61.0, 99.0, 61.0])
        self.assertEqual(again["updated"], "2026-10-08T04:30:00Z")

    def test_the_file_is_one_record_per_line_ascii_and_round_trips(self):
        doc = bh.merge(bh.merge(None, self.record("2026-10-07"), NOW), self.record("2026-10-08"), NOW)
        text = bh.dump(doc)
        text.encode("ascii")
        lines = text.splitlines()
        self.assertEqual(sum(1 for l in lines if l.startswith('{"date"')), 2)
        self.assertEqual(json.loads(text), doc)
        self.assertTrue(text.endswith("\n"))


class CliTests(unittest.TestCase):
    def test_records_a_day_and_replaces_it_on_a_rerun(self):
        with Sandbox() as sb:
            bench, check = sb.write("bench.json", main_doc()), sb.write("check.json", check_doc())
            out = sb.dir / "data" / "build-history.json"
            args = ["--bench", bench, "--bench", check, "--in", str(out), "--out", str(out), "--date", "2026-10-08", "--run-id", "5"]
            first = sb.run(*args)
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertIn("recorded 2026-10-08 at 0123456789ab: 9 rows", first.stdout)
            second = sb.run(*args)
            self.assertEqual(second.returncode, 0, second.stderr)
            doc = json.loads(out.read_text())
            self.assertEqual(len(doc["records"]), 1)
            self.assertEqual(doc["records"][0]["workflow_run"], 5)

    def test_a_failed_benchmark_leaves_the_file_exactly_as_it_was(self):
        with Sandbox() as sb:
            good = sb.write("good.json", main_doc())
            out = sb.dir / "build-history.json"
            self.assertEqual(sb.run("--bench", good, "--out", str(out), "--date", "2026-10-07").returncode, 0)
            before = out.read_bytes()
            bad = sb.write("bad.json", main_doc(failure="leaf edit-rebuild: cargo exited 101"))
            proc = sb.run("--bench", bad, "--in", str(out), "--out", str(out), "--date", "2026-10-08")
            self.assertEqual(proc.returncode, 1)
            self.assertIn("nothing recorded", proc.stderr)
            self.assertEqual(out.read_bytes(), before)
            fresh = sb.dir / "fresh.json"
            self.assertEqual(sb.run("--bench", bad, "--out", str(fresh)).returncode, 1)
            self.assertFalse(fresh.exists())

    def test_a_red_check_still_records_the_day(self):
        with Sandbox() as sb:
            out = sb.dir / "h.json"
            proc = sb.run("--bench", sb.write("b.json", main_doc()), "--bench", sb.write("c.json", check_doc(status="failed")),
                          "--out", str(out), "--date", "2026-10-08")
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIn("failed: check", proc.stdout)
            self.assertEqual(json.loads(out.read_text())["records"][0]["failed"], ["check"])

    def test_a_data_file_that_is_not_ours_is_not_overwritten(self):
        with Sandbox() as sb:
            out = sb.dir / "h.json"
            out.write_text("not json at all")
            proc = sb.run("--bench", sb.write("b.json", main_doc()), "--in", str(out), "--out", str(out))
            self.assertEqual(proc.returncode, 2)
            self.assertEqual(out.read_text(), "not json at all")
            out.write_text('{"something": "else"}')
            self.assertEqual(sb.run("--bench", sb.write("b2.json", main_doc()), "--in", str(out), "--out", str(out)).returncode, 2)

    def test_bad_arguments_exit_2(self):
        with Sandbox() as sb:
            out = str(sb.dir / "h.json")
            b = sb.write("b.json", main_doc())
            self.assertEqual(sb.run("--bench", str(sb.dir / "missing.json"), "--out", out).returncode, 2)
            self.assertEqual(sb.run("--bench", b, "--out", out, "--date", "yesterday").returncode, 2)
            self.assertEqual(sb.run("--bench", b, "--out", out, "--commit-env").returncode, 2)

    def test_commit_env_takes_github_sha(self):
        with Sandbox() as sb:
            out = sb.dir / "h.json"
            proc = sb.run("--bench", sb.write("b.json", main_doc()), "--out", str(out), "--commit-env",
                          env={"GITHUB_SHA": "cafebabecafebabecafebabe00"})
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertEqual(json.loads(out.read_text())["records"][0]["commit"], "cafebabecafe")


class WiringTests(unittest.TestCase):
    def test_both_suites_run_in_make_check(self):
        cmds = check_steps.commands()
        self.assertIn("python3 ci/test_build_history.py", cmds)
        self.assertIn("python3 ci/test_build_bench.py", cmds)


if __name__ == "__main__":
    unittest.main()
