#!/usr/bin/env python3
"""Adversarial tests for the independent exact finite-episode telemetry grader.

The useful cases inject a lie into one field and require the grader to disagree. A green test that
only feeds the grader honest output would not distinguish a proof checker from a parser matching no
lines at all.
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


def sample(prod_pm, dur_ms=2000, buf_ms=8000, complete=1):
    buf = f"{buf_ms}ms" if buf_ms != "none" else "none"
    complete_field = "" if complete is None else f" complete={complete}"
    return (
        f"abr: sample current=4000kbps media=3000kbps net=9000kbps buf={buf} "
        f"vbuf=8000ms abuf=8000ms dur={dur_ms}ms prod={prod_pm}pm n=9 "
        f"decision=stay target=0kbps{complete_field} reason=None"
    )


def window(have, demand=None, supply=None, excess=None, runway=None, *, sus=1, sur=1,
           verdict=None, want=None, byte_count=1000, dur_ms=2000, resets=0, eps=0,
           clamp=0, bound=-1):
    want = have if want is None else want
    if have == 0:
        demand = supply = excess = runway = -1
        sus = sur = 0
        verdict = "filling" if verdict is None else verdict
    else:
        assert None not in (demand, supply, excess, runway)
        verdict = ("admit" if sus and sur else "refuse") if verdict is None else verdict
    return (
        f"abr: window current=4000kbps verdict={verdict} have={have}/{want} eps={eps}pm "
        f"clamp={clamp} bound={bound}ms demand={demand}ms supply={supply}ms "
        f"excess={excess}ms runway={runway}ms sus={sus} sur={sur} reset={resets} "
        f"bytes={byte_count} dur={dur_ms}ms"
    )


def seed():
    return "abr: seed rung=4000kbps prior=none slow=4000kbps fast=4000kbps unc=500pm n=0 pin=none"


def commit_marker(direction="Up", to_kbps=None):
    if to_kbps is None:
        to_kbps = 6000 if direction == "Up" else 4000
    return f"abr: committed {direction} to {to_kbps}kbps 1920x1080 out=1918x802"


def transaction(direction="Up", outcome="committed", candidate_acq=900,
                candidate_bytes=1441792, candidate_dur=2000):
    from_kbps, to_kbps = ((4000, 6000) if direction == "Up" else (6000, 4000))
    return (
        f"abr: tx {direction} {from_kbps}->{to_kbps}kbps outcome={outcome} "
        "decided=3065ms total=4100ms control=120ms prime=40ms master=30ms media=50ms "
        "warmup=1800ms graded=900ms warmup_dl=2200ms buf_start=9000ms "
        "buf_decided=6000ms feed=900ms buf_fed=9000ms buf_end=9000ms "
        "cur_acq_before=1200ms net=9000kbps fast=9200kbps slow=8800kbps unc=120pm "
        f"declared=5602kbps graded_bytes=1441792 candidate_acq={candidate_acq}ms "
        f"candidate_bytes={candidate_bytes} candidate_dur={candidate_dur}ms"
    )


def graded(lines):
    printed = io.StringIO()
    with redirect_stdout(printed):
        result = wg.grade(wg.paired(list(lines)))
    return result, printed.getvalue()


def exact_flat_run(count=9):
    lines = []
    for n in range(1, count + 1):
        lines.extend([
            sample(500), window(n, n * 1000, n * 2000, 0, 1000),
        ])
    return lines


class Pairing(unittest.TestCase):
    def test_window_without_sample_is_an_error(self):
        with self.assertRaises(SystemExit):
            wg.paired([window(0)])

    def test_two_samples_without_window_are_an_error(self):
        with self.assertRaises(SystemExit):
            wg.paired([sample(500), sample(500)])

    def test_unknown_reserve_remains_none(self):
        rows = wg.paired([sample(500, buf_ms="none"), window(1, 1000, 2000, 0, 1000)])
        self.assertIsNone(rows[0][1]["buf_ms"])

    def test_window_fields_are_named_by_the_harness_contract(self):
        row = wg.paired([sample(500), window(1, 1000, 2000, 0, 1000)])[0][2]
        self.assertEqual((row["demand_ms"], row["runway_ms"], row["sus"], row["sur"]),
                         (1000, 1000, 1, 1))


class Quantisation(unittest.TestCase):
    def test_prod_is_an_exact_half_open_interval(self):
        self.assertEqual(wg.acquisition_interval({"prod_pm": 500, "dur_ms": 2000}),
                         (1_000_000, 1_002_000))

    def test_zero_prod_still_means_a_positive_transfer(self):
        self.assertEqual(wg.acquisition_interval({"prod_pm": 0, "dur_ms": 2000}), (1, 2_000))

    def test_saturated_prod_has_no_fictitious_upper_endpoint(self):
        lo, hi = wg.acquisition_interval({"prod_pm": 2**32 - 1, "dur_ms": 1})
        self.assertEqual(lo, 2**32 - 1)
        self.assertIsNone(hi)


class ExactFiniteEpisode(unittest.TestCase):
    def test_one_sample_runway_includes_the_terminal_acquisition(self):
        result, printed = graded([sample(500), window(1, 1000, 2000, 0, 1000)])
        self.assertEqual(result["disagree"], 0, printed)

    def test_each_observation_keeps_its_own_duration(self):
        result, printed = graded([
            sample(1200, dur_ms=1500),
            window(1, 1800, 1500, 300, 1800, sus=0, dur_ms=1500),
            sample(400, dur_ms=2500),
            window(2, 2800, 4000, 300, 1800, dur_ms=2500),
        ])
        self.assertEqual(result["disagree"], 0, printed)

    def test_window_duration_cannot_define_its_own_expected_supply(self):
        result, printed = graded([
            sample(500, dur_ms=1500), window(1, 750, 1500, 0, 750, dur_ms=2000),
        ])
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("sample dur", printed)

    def test_abandoned_prefix_is_excluded_but_its_reserve_is_current(self):
        result, printed = graded([
            sample(500, buf_ms=2000), window(1, 1000, 2000, 0, 1000),
            sample(500, buf_ms=500, complete=0),
            window(1, 1000, 2000, 0, 1000, sur=0),
        ])
        self.assertEqual(result["disagree"], 0, printed)

    def test_unknown_reserve_appends_sample_and_reuses_last_observation(self):
        result, printed = graded([
            sample(500, buf_ms=8000), window(1, 1000, 2000, 0, 1000),
            sample(500, buf_ms="none"), window(2, 2000, 4000, 0, 1000),
        ])
        self.assertEqual(result["disagree"], 0, printed)

    def test_window_bytes_cannot_choose_membership(self):
        result, printed = graded([
            sample(500), window(1, 1000, 2000, 0, 1000, byte_count=0),
        ])
        self.assertEqual(result["disagree"], 0, printed)

    def test_missing_complete_bit_is_incompatible_and_not_guessed(self):
        result, printed = graded([
            sample(500, complete=None), window(1, 1000, 2000, 0, 1000),
        ])
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertEqual(result["ungraded"], 1)
        self.assertIn("no complete=", printed)

    def test_empty_episode_has_the_only_legal_filling_shape(self):
        result, printed = graded([sample(500, complete=0), window(0)])
        self.assertEqual((result["disagree"], result["filling"]), (0, 1), printed)

    def test_missing_terminal_cost_is_caught(self):
        result, printed = graded([sample(500), window(1, 1000, 2000, 0, 0)])
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("runway", printed)

    def test_reserve_below_runway_forces_refusal(self):
        result, printed = graded([
            sample(500, buf_ms=999), window(1, 1000, 2000, 0, 1000, sur=0),
        ])
        self.assertEqual(result["disagree"], 0, printed)

    def test_live_episode_keeps_all_entries_after_the_diagnostic_ring_would_wrap(self):
        result, printed = graded(exact_flat_run(65))
        self.assertEqual(result["disagree"], 0, printed)
        self.assertEqual(result["checked"], 65)


class IndependentFailureDetection(unittest.TestCase):
    def test_bad_eps_does_not_disable_finite_arithmetic(self):
        result, printed = graded([
            sample(500), window(1, 999, 2000, 0, 1000, eps=100),
        ])
        self.assertGreaterEqual(result["disagree"], 2)
        self.assertIn("eps=100", printed)
        self.assertIn("demand", printed)

    def test_bad_want_cannot_select_a_self_confirming_subset(self):
        result, printed = graded([
            sample(500), window(1, 1000, 2000, 0, 1000),
            sample(500), window(2, 1000, 4000, 0, 1000, want=1),
        ])
        self.assertGreaterEqual(result["disagree"], 2)
        self.assertIn("want=1", printed)
        self.assertIn("demand", printed)

    def test_have_not_justified_by_events_is_caught(self):
        lines = exact_flat_run(2)
        lines[-1] = window(3, 2000, 4000, 0, 1000)
        result, printed = graded(lines)
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("certified event stream", printed)

    def test_wrong_supply_excess_and_runway_are_each_caught(self):
        for field, bad in (("supply", (1000, 1900, 0, 1000)),
                           ("excess", (1000, 2000, 10, 1000)),
                           ("runway", (1000, 2000, 0, 999))):
            with self.subTest(field=field):
                result, printed = graded([sample(500), window(1, *bad)])
                self.assertGreaterEqual(result["disagree"], 1)
                self.assertIn(field, printed)

    def test_forced_sustainability_flag_is_checked(self):
        result, printed = graded([
            sample(500), window(1, 1000, 2000, 0, 1000, sus=0),
        ])
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("sustainable", printed)

    def test_wire_verdict_always_matches_logged_boolean_pair(self):
        result, printed = graded([
            sample(500), window(1, 1000, 2000, 0, 1000, verdict="refuse"),
        ])
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("requires admit", printed)


class ThresholdCoverage(unittest.TestCase):
    def test_sustainability_straddling_quantisation_is_counted_ambiguous(self):
        result, printed = graded([
            sample(1000), window(1, 2000, 2000, 0, 2000),
        ])
        self.assertEqual(result["disagree"], 0, printed)
        self.assertEqual(result["ambiguous_sus"], 1)

    def test_survival_straddling_quantisation_is_counted_ambiguous(self):
        result, printed = graded([
            sample(500, buf_ms=1001), window(1, 1001, 2000, 0, 1001),
        ])
        self.assertEqual(result["disagree"], 0, printed)
        self.assertEqual(result["ambiguous_sur"], 1)

    def test_saturated_ratio_is_reported_ungraded_not_fully_checked(self):
        result, printed = graded([
            sample(2**32 - 1, dur_ms=1, buf_ms=0),
            window(1, 4294967, 1, 4294966, 4294967, sus=0, sur=0, dur_ms=1),
        ])
        self.assertEqual(result["disagree"], 0, printed)
        self.assertEqual((result["checked"], result["ungraded"], result["saturated"]),
                         (0, 1, 1))


class CommitCertificate(unittest.TestCase):
    def test_matching_marker_and_transaction_seed_one_candidate(self):
        rows = wg.paired([commit_marker(), transaction()])
        self.assertEqual([row[0] for row in rows], ["commit"])
        self.assertEqual(rows[0][1], {
            "acq_lo_us": 900_000, "acq_hi_us": 901_000, "dur_us": 2_000_000,
        })

    def test_zero_millisecond_candidate_is_a_valid_positive_interval(self):
        rows = wg.paired([commit_marker(), transaction(candidate_acq=0)])
        self.assertEqual((rows[0][1]["acq_lo_us"], rows[0][1]["acq_hi_us"]), (1, 1000))

    def test_committed_outcome_without_marker_never_seeds(self):
        result, printed = graded([transaction()])
        self.assertEqual(result["candidates"], 0)
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("no preceding commit certificate", printed)

    def test_marker_and_transaction_must_match_direction_and_target(self):
        result, printed = graded([commit_marker("Down"), transaction("Up")])
        self.assertEqual(result["candidates"], 0)
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("disagrees", printed)

    def test_rejected_candidate_does_not_touch_the_episode(self):
        self.assertEqual(wg.paired([transaction(outcome="not_ready")]), [])

    def test_commit_marker_followed_by_rejection_is_a_hard_error(self):
        result, printed = graded([commit_marker(), transaction(outcome="not_ready")])
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("outcome=not_ready", printed)

    def test_commit_requires_complete_candidate_triple(self):
        result, printed = graded([
            commit_marker(), transaction(candidate_acq=-1, candidate_bytes=-1, candidate_dur=-1),
        ])
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("no complete candidate evidence", printed)

    def test_up_and_down_commit_replace_then_extend_the_episode(self):
        for direction in ("Up", "Down"):
            with self.subTest(direction=direction):
                result, printed = graded([
                    sample(500), window(1, 1000, 2000, 0, 1000),
                    commit_marker(direction), transaction(direction),
                    sample(500), window(2, 1900, 4000, 0, 1000, resets=1),
                ])
                self.assertEqual(result["disagree"], 0, printed)
                self.assertEqual((result["candidates"], result["resets"]), (1, 1))


class EpochAndReset(unittest.TestCase):
    def test_seed_starts_a_new_epoch_and_resets_reserve_to_zero(self):
        result, printed = graded([
            sample(500, buf_ms=8000), window(1, 1000, 2000, 0, 1000), seed(),
            sample(500, buf_ms="none"), window(1, 1000, 2000, 0, 1000, sur=0),
        ])
        self.assertEqual(result["disagree"], 0, printed)
        self.assertEqual(result["epochs"], 1)

    def test_reset_without_commit_is_not_a_benign_resync(self):
        result, printed = graded([
            sample(500), window(1, 1000, 2000, 0, 1000, resets=1),
        ])
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("no commit evidence", printed)

    def test_multi_step_reset_jump_with_one_commit_is_caught(self):
        result, printed = graded([
            commit_marker(), transaction(),
            sample(500), window(2, 1900, 4000, 0, 1000, resets=2),
        ])
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("expected exactly 1", printed)

    def test_counter_going_backwards_is_caught(self):
        result, printed = graded([
            commit_marker(), transaction(),
            sample(500), window(2, 1900, 4000, 0, 1000, resets=1),
            sample(500), window(3, 2900, 6000, 0, 1000, resets=0),
        ])
        self.assertGreaterEqual(result["disagree"], 1)
        self.assertIn("BACKWARDS", printed)

    def test_seed_makes_counter_zero_legal_again(self):
        result, printed = graded([
            commit_marker(), transaction(),
            sample(500), window(2, 1900, 4000, 0, 1000, resets=1), seed(),
            sample(500), window(1, 1000, 2000, 0, 1000),
        ])
        self.assertEqual(result["disagree"], 0, printed)


class HonestRunAndOccupancy(unittest.TestCase):
    def test_flat_run_is_fully_graded(self):
        result, printed = graded(exact_flat_run())
        self.assertEqual(result["disagree"], 0, printed)
        self.assertEqual((result["checked"], result["filling"]), (9, 0))

    def test_both_quantisation_endpoints_are_accepted(self):
        for demand in (9000, 9017):
            with self.subTest(demand=demand):
                lines = exact_flat_run()
                lines[-1] = window(9, demand, 18000, 0, 1001 if demand == 9017 else 1000)
                result, printed = graded(lines)
                self.assertEqual(result["disagree"], 0, printed)

    def test_occupancy_describes_what_was_exercised(self):
        occ = wg.occupancy(wg.paired(exact_flat_run()))
        self.assertEqual(occ["graded"], 9)
        self.assertEqual(occ["verdicts"], {"admit": 9, "refuse": 0})
        self.assertAlmostEqual(occ["load_mean"], 0.5)
        self.assertEqual(occ["excess_nonzero"], 0)

    def test_only_empty_episode_reports_no_occupancy(self):
        self.assertIsNone(wg.occupancy(wg.paired([sample(500, complete=0), window(0)])))


class CommandExitStatus(unittest.TestCase):
    def run_main(self, lines):
        with tempfile.NamedTemporaryFile("w", suffix=".log") as trace:
            trace.write("\n".join(lines))
            trace.flush()
            printed = io.StringIO()
            with redirect_stdout(printed):
                status = wg.main([trace.name])
            return status, printed.getvalue()

    def test_a_fully_graded_trace_exits_successfully(self):
        status, printed = self.run_main(exact_flat_run(2))
        self.assertEqual(status, 0, printed)

    def test_no_current_trace_is_a_failure_not_a_clean_zero(self):
        status, printed = self.run_main(["an unrelated old log line"])
        self.assertEqual(status, 1)
        self.assertIn("NO TRACE", printed)

    def test_ungraded_saturation_makes_the_command_fail(self):
        status, printed = self.run_main([
            sample(2**32 - 1, dur_ms=1, buf_ms=0),
            window(1, 4294967, 1, 4294966, 4294967, sus=0, sur=0, dur_ms=1),
        ])
        self.assertEqual(status, 1)
        self.assertIn("1 ungraded", printed)


if __name__ == "__main__":
    unittest.main(verbosity=2)
