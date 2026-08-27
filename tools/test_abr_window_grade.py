#!/usr/bin/env python3
"""Tests for `tools/abr-window-grade.py`.

The grader's whole value is that it disagrees with the app when the app is wrong, so the tests that
matter are the ones that INJECT a wrong line and check it is caught. A grader that only ever reports
zero disagreements is indistinguishable from one that parses nothing, and that failure mode is the
realistic one here: the app writes these lines on a television and this runs on a Mac, so "no
disagreements" and "no lines" look identical in the summary.
"""

from __future__ import annotations

import importlib.util
import io
import pathlib
import tempfile
import unittest
from contextlib import redirect_stdout

_spec = importlib.util.spec_from_file_location(
    "abr_window_grade", pathlib.Path(__file__).with_name("abr-window-grade.py")
)
assert _spec and _spec.loader
wg = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(wg)


def sample(prod_pm, dur_ms=2000, buf_ms=8000):
    return (f"abr: sample current=4000kbps media=3000kbps net=9000kbps buf={buf_ms}ms "
            f"vbuf={buf_ms}ms abuf={buf_ms}ms dur={dur_ms}ms prod={prod_pm}pm n=9 "
            f"decision=stay target=0kbps")


def window(verdict, have, want, bound, demand, supply, excess, sus, sur,
           byte_count=1000, dur_ms=2000, resets=0):
    return (f"abr: window current=4000kbps verdict={verdict} have={have}/{want} eps=100pm "
            f"clamp=0 bound={bound}ms demand={demand}ms supply={supply}ms excess={excess}ms "
            f"sus={sus} sur={sur} reset={resets} bytes={byte_count} dur={dur_ms}ms")


def graded(lines):
    """(result, printed) for one synthetic log."""
    with tempfile.TemporaryDirectory() as directory:
        path = pathlib.Path(directory) / "case.log"
        path.write_text("\n".join(lines) + "\n")
        buf = io.StringIO()
        with redirect_stdout(buf):
            result = wg.grade(str(path))
        return result, buf.getvalue()


class Transfer(unittest.TestCase):
    def test_the_ceiling_matches_the_shipped_form(self):
        # Differential against truncation: 100*10//7 is 142, the ceiling is 143.
        self.assertEqual(wg.transferred_us(7, 100, 10), 143)
        self.assertEqual(100 * 10 // 7, 142)

    def test_a_downshift_query_is_flat(self):
        self.assertEqual(wg.transferred_us(1000, 100, 500), 100)
        self.assertEqual(wg.transferred_us(1000, 100, 1000), 100)


class Pairing(unittest.TestCase):
    def test_a_window_line_with_no_sample_before_it_is_an_error(self):
        # Silent mis-pairing would attribute every number to the wrong segment, and every check
        # downstream would still "pass" against the neighbouring segment's numbers.
        with self.assertRaises(SystemExit):
            graded([window("filling", 1, 9, -1, -1, -1, -1, 0, 0)])

    def test_pairs_come_out_in_order(self):
        rows = wg.paired([sample(500), window("filling", 1, 9, -1, -1, -1, -1, 0, 0),
                          sample(600), window("filling", 2, 9, -1, -1, -1, -1, 0, 0)])
        self.assertEqual([r[2]["have"] for r in rows], [1, 2])
        self.assertEqual([r[1]["prod_pm"] for r in rows], [500, 600])


class TheQuantizationInterval(unittest.TestCase):
    def test_a_truncated_prod_admits_one_duration_of_acquisition(self):
        # prod = total_fetch_us / dur_ms truncated, so prod=500 at dur=2000 means the acquisition
        # was in [1_000_000, 1_002_000) us. Any tolerance narrower than this would report the app
        # as wrong for rounding correctly.
        self.assertEqual(wg.acquisition_interval({"prod_pm": 500, "dur_ms": 2000}),
                         (1_000_000, 1_002_000))


class AnHonestLogIsAccepted(unittest.TestCase):
    """A flat window whose numbers the app could really have produced."""

    @classmethod
    def build(cls, demand=None, excess=0, sus=1):
        # Nine identical segments at prod=500pm (1.000-1.002 s each) against a 2 s duration.
        lines, n = [], 9
        for i in range(n):
            lines.append(sample(500))
            if i < n - 1:
                lines.append(window("filling", i + 1, n, -1, -1, -1, -1, 0, 0))
        # 9 x [1000, 1002) ms of demand against 9 x 2000 ms of supply.
        lines.append(window("admit", n, n, 1001, 9009 if demand is None else demand,
                            18000, excess, sus, 1))
        return lines

    def test_a_consistent_log_reports_no_disagreement(self):
        result, printed = graded(self.build())
        self.assertEqual(result["disagree"], 0, printed)
        self.assertEqual(result["checked"], 1)
        self.assertEqual(result["filling"], 8)

    def test_both_ends_of_the_interval_are_accepted(self):
        for demand in (9000, 9017):        # 9 x 1000 and 9 x 1001 (floor of 1001.999)
            with self.subTest(demand=demand):
                result, printed = graded(self.build(demand=demand))
                self.assertEqual(result["disagree"], 0, printed)


class AWrongLogIsCaught(unittest.TestCase):
    """Each of these is a way the shipped arithmetic could be wrong, injected one at a time."""

    def test_a_demand_outside_the_interval_is_caught(self):
        result, printed = graded(AnHonestLogIsAccepted.build(demand=8999))
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("demand", printed)

    def test_a_supply_that_is_not_n_times_d_is_caught(self):
        lines = AnHonestLogIsAccepted.build()
        lines[-1] = window("admit", 9, 9, 1001, 9009, 16000, 0, 1, 1)
        result, printed = graded(lines)
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("supply", printed)

    def test_an_excess_the_segments_cannot_produce_is_caught(self):
        # Every segment is under the 2 s duration, so `sum (T_i - D)+` is exactly zero.
        result, printed = graded(AnHonestLogIsAccepted.build(excess=400))
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("excess", printed)

    def test_a_sustainability_flag_contradicting_its_own_sums_is_caught(self):
        result, printed = graded(AnHonestLogIsAccepted.build(sus=0))
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("sustainable", printed)

    def test_a_have_count_the_segment_stream_cannot_justify_is_caught(self):
        # The check that the shadow SAW the same segments: this is what would catch an `observe`
        # placed below an early return, which is the exact defect the shipped `safe_budget`
        # already suffered on 397 of 527 lines.
        lines = AnHonestLogIsAccepted.build()
        lines[-1] = window("admit", 4, 9, 1001, 9009, 18000, 0, 1, 1)
        result, printed = graded(lines)
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("have", printed)

    def test_a_full_window_still_claiming_to_be_filling_is_caught(self):
        lines = AnHonestLogIsAccepted.build()
        lines[-1] = window("filling", 9, 9, -1, -1, -1, -1, 0, 0)
        result, printed = graded(lines)
        self.assertGreaterEqual(result["disagree"], 1)


class Occupancy(unittest.TestCase):
    """The half a pass/fail cannot report: what the run actually exercised."""

    def test_an_idle_link_is_reported_as_an_idle_link(self):
        occ = None
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "case.log"
            path.write_text("\n".join(AnHonestLogIsAccepted.build()) + "\n")
            occ = wg.occupancy(str(path))
        self.assertEqual(occ["graded"], 1)
        self.assertEqual(occ["verdicts"], {"admit": 1, "refuse": 0})
        self.assertAlmostEqual(occ["load_mean"], 9009 / 18000)
        self.assertEqual(occ["excess_nonzero"], 0)

    def test_a_log_with_nothing_graded_reports_none_rather_than_a_zero(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "case.log"
            path.write_text(sample(500) + "\n"
                            + window("filling", 1, 9, -1, -1, -1, -1, 0, 0) + "\n")
            self.assertIsNone(wg.occupancy(str(path)))


class TheResetPath(unittest.TestCase):
    """A delivery collapse clears the window, and the grader cannot see a collapse.

    So it replays the reset from the app's own monotone counter. That has to be free when the
    counter moves and caught when it does not -- otherwise the grader either reports every
    collapsing run as broken (which it did, on `pipe_abr_down_collapse`, before the counter
    existed) or stops noticing a window that lost its history for no stated reason.
    """

    @staticmethod
    def run_of(reset_at=None, reset_value=1):
        """Twelve segments; optionally the window resets before segment `reset_at`."""
        lines, resets = [], 0
        have = 0
        for i in range(12):
            if reset_at is not None and i == reset_at:
                resets = reset_value
                have = 0
            have += 1
            lines.append(sample(500))
            lines.append(window("filling", have, 19, -1, -1, -1, -1, 0, 0, resets=resets))
        return lines

    def test_a_reset_the_counter_accounts_for_costs_no_disagreement(self):
        result, printed = graded(self.run_of(reset_at=6))
        self.assertEqual(result["disagree"], 0, printed)
        self.assertEqual(result["resets"], 1)

    def test_a_run_with_no_reset_reports_none(self):
        result, printed = graded(self.run_of())
        self.assertEqual((result["disagree"], result["resets"]), (0, 0), printed)

    def test_a_have_that_drops_without_the_counter_moving_is_still_caught(self):
        # The check the counter must not weaken. Before it existed this was the ONLY signal, and
        # keying on the counter would be worthless if it swallowed this case too.
        lines = self.run_of()
        lines[13] = window("filling", 1, 19, -1, -1, -1, -1, 0, 0)   # segment 7 restarts at 1
        result, printed = graded(lines)
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("have", printed)

    def test_a_counter_going_backwards_is_caught(self):
        # Monotone is the whole contract: a counter that can fall could hide a reset by
        # cancelling itself out across two segments.
        # Segment 8's window line: index 2*8+1. Even indices are `abr: sample`, and overwriting
        # one of those would break the pairing instead of testing the counter.
        lines = self.run_of(reset_at=4, reset_value=3)
        lines[17] = window("filling", 5, 19, -1, -1, -1, -1, 0, 0, resets=1)
        result, printed = graded(lines)
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("BACKWARDS", printed)

    def test_a_multi_step_jump_in_the_counter_is_one_resync_not_a_failure(self):
        # Two collapses between two logged segments is legal (nothing promises one per segment).
        result, printed = graded(self.run_of(reset_at=6, reset_value=2))
        self.assertEqual(result["disagree"], 0, printed)
        self.assertEqual(result["resets"], 2)


class TheCandidateObservation(unittest.TestCase):
    """A transaction adds ONE sample to the window that no `abr: window` line describes.

    `Controller::observe_candidate` puts the graded candidate segment in, and every `abr: window`
    line is a CURRENT-stream segment — so a replayer that only reads those counts one short after
    every transaction, forever. That is not the app miscounting, and before `graded_bytes=` reached
    the wire this grader reported 54 disagreements on a healthy 15-case run.
    """

    TX = ("abr: tx Up 4000->6000kbps outcome=committed decided=3065ms total=4100ms control=120ms "
          "prime=40ms master=30ms media=50ms warmup=1800ms graded=900ms buf_start=9000ms "
          "buf_decided=6000ms feed=900ms buf_fed=9000ms buf_end=9000ms cur_acq_before=1200ms "
          "net=9000kbps fast=9200kbps slow=8800kbps unc=120pm declared=5602kbps "
          "graded_bytes=1441792")

    def test_a_transaction_line_contributes_one_observation(self):
        rows = wg.paired([sample(500), window("filling", 1, 9, -1, -1, -1, -1, 0, 0), self.TX])
        self.assertEqual([r[0] for r in rows], ["segment", "candidate"])
        self.assertEqual(rows[1], ("candidate", 1441792, 900_000))

    def test_it_lands_between_the_windows_either_side_of_it(self):
        """Exact, not approximate: the transaction runs inline on the demux worker, so no
        current-stream segment is acquired while it is in flight."""
        rows = wg.paired([sample(500), window("filling", 1, 9, -1, -1, -1, -1, 0, 0),
                          self.TX,
                          sample(600), window("filling", 3, 9, -1, -1, -1, -1, 0, 0)])
        self.assertEqual([r[0] for r in rows], ["segment", "candidate", "segment"])

    def test_the_run_grades_clean_when_the_candidate_is_accounted_for(self):
        lines = []
        for i in range(9):
            lines.append(sample(500))
            lines.append(window("filling", i + 1, 19, -1, -1, -1, -1, 0, 0))
        lines.append(self.TX)
        lines.append(sample(500))
        lines.append(window("filling", 11, 19, -1, -1, -1, -1, 0, 0))
        result, printed = graded(lines)
        self.assertEqual(result["disagree"], 0, printed)
        self.assertEqual(result["candidates"], 1)

    def test_an_unaccounted_extra_sample_is_still_caught(self):
        """The check the splice must not weaken: a `have` that jumps with no transaction to
        explain it is the app miscounting, and that has to keep failing."""
        lines = [sample(500), window("filling", 1, 19, -1, -1, -1, -1, 0, 0),
                 sample(500), window("filling", 3, 19, -1, -1, -1, -1, 0, 0)]
        result, printed = graded(lines)
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("have", printed)

    def test_a_rejected_transaction_still_contributes_its_observation(self):
        """A rejected candidate MEASURED the link, so its graded segment is in the window too."""
        rejected = self.TX.replace("outcome=committed", "outcome=not_ready")
        rows = wg.paired([rejected])
        self.assertEqual(rows[0][0], "candidate")


if __name__ == "__main__":
    unittest.main(verbosity=2)
