#!/usr/bin/env python3
"""Tests for `tools/abr-transfer-bound.py`.

The tool's headline result is a REFUTATION followed by a validation, so the tests that matter are
the ones that would still fail if the transfer bound were written the wrong way round -- which is
the easy mistake, because the two directions are asymmetric and only one of them needs a size
prediction at all.
"""

from __future__ import annotations

import importlib.util
import pathlib
import unittest

_spec = importlib.util.spec_from_file_location(
    "abr_transfer_bound", pathlib.Path(__file__).with_name("abr-transfer-bound.py")
)
assert _spec and _spec.loader
tb = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(tb)


class Transfer(unittest.TestCase):
    def test_upshift_scales_with_bytes(self):
        # tau <= A_i/b_i, attained at O0 = 0, so twice the bytes costs at most twice the time.
        self.assertEqual(tb.transfer(100, 1000, 2000), 200.0)

    def test_downshift_does_not_fall(self):
        # tau >= 0, attained at tau = 0: fewer bytes may cost the same, never provably less.
        self.assertEqual(tb.transfer(100, 1000, 500), 100.0)

    def test_downshift_needs_no_size_prediction(self):
        # The property that lets rungs 320/720/2000 keep working without a usable `sigma`:
        # every query below the observed size gives the SAME bound.
        bounds = {tb.transfer(100, 1000, q) for q in (1, 10, 500, 999, 1000)}
        self.assertEqual(bounds, {100.0})

    def test_equal_bytes_is_identity(self):
        self.assertEqual(tb.transfer(137, 4096, 4096), 137.0)

    def test_zero_bytes_does_not_divide(self):
        # A malformed log line must not take the demux worker's arithmetic with it.
        self.assertEqual(tb.transfer(42, 0, 9999), 42.0)


class ReadSegments(unittest.TestCase):
    def test_parses_and_drops_cold_start(self):
        text = (
            "hls: segment=0 bytes=100 raster=1920x1080 open_ms=2 ttfb_ms=0 "
            "open_probe_ms=7 first_au_ms=8 total_ms=900\n"
            "hls: segment=1 bytes=200 raster=1920x1080 open_ms=2 ttfb_ms=0 "
            "open_probe_ms=7 first_au_ms=8 total_ms=500\n"
            "unrelated line\n"
        )
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            log = pathlib.Path(directory) / "case.log"
            log.write_text(text)
            self.assertEqual(tb.read_segments(str(log)), [(200, 500)])


class OrderStatistic(unittest.TestCase):
    def test_kth_largest_is_the_bound(self):
        # Window of three identical-size observations; k=1 takes the max, so the next value
        # exceeds only if it is above every one of them.
        segments = [(1000, 10), (1000, 20), (1000, 30), (1000, 25)]
        total, exceed, _ = tb.grade_order(segments, window=3, k=1)
        self.assertEqual((total, exceed), (1, 0))

    def test_exceedance_is_counted(self):
        segments = [(1000, 10), (1000, 20), (1000, 30), (1000, 31)]
        total, exceed, worst = tb.grade_order(segments, window=3, k=1)
        self.assertEqual((total, exceed), (1, 1))
        self.assertAlmostEqual(worst, 31 / 30)

    def test_larger_k_is_a_tighter_bound(self):
        # k=2 takes the SECOND largest, so it must exceed at least as often as k=1.
        segments = [(1000, v) for v in (10, 20, 30, 25, 28, 22, 26)]
        _, exceed_1, _ = tb.grade_order(segments, window=3, k=1)
        _, exceed_2, _ = tb.grade_order(segments, window=3, k=2)
        self.assertLessEqual(exceed_1, exceed_2)

    def test_window_longer_than_history_tests_nothing(self):
        total, exceed, _ = tb.grade_order([(1, 1), (2, 2)], window=10, k=1)
        self.assertEqual((total, exceed), (0, 0))


class Pairs(unittest.TestCase):
    def test_consistent_plant_never_violates(self):
        # Points generated from an exact A = O0 + b*tau lie ON the bound's feasible set, so a
        # deterministic plant must produce zero violations. This is the differential test: it
        # fails if `transfer` drops the max(1, .) and lets a downshift predict a smaller time.
        o0, tau = 300, 2  # ms, ms per byte
        segments = [(b, o0 + b * tau) for b in (100, 200, 400, 800)]
        total, violations, worst = tb.grade_pairs(segments)
        self.assertEqual(violations, 0)
        self.assertEqual(total, 12)
        self.assertEqual(worst, 1.0)

    def test_pure_fixed_cost_never_violates(self):
        segments = [(b, 500) for b in (100, 900, 4000)]
        _, violations, _ = tb.grade_pairs(segments)
        self.assertEqual(violations, 0)

    def test_superlinear_cost_does_violate(self):
        # Cost growing FASTER than linearly in bytes breaks the tau <= A_i/b_i ceiling, which is
        # the one way the model can be wrong that the bound cannot absorb.
        segments = [(100, 10), (1000, 1000)]
        _, violations, worst = tb.grade_pairs(segments)
        self.assertEqual(violations, 1)
        self.assertAlmostEqual(worst, 10.0)


class Climb(unittest.TestCase):
    def test_healthy_link_admits_a_large_ratio(self):
        # A/D = 0.25 with no dispersion: the bound should admit close to a 4x byte ratio.
        segments = [(1000, 500)] * 25
        load, ratio = tb.grade_climb(segments, window=20, k=1, duration_ms=2000)
        self.assertAlmostEqual(load, 0.25)
        self.assertAlmostEqual(ratio, 4.0, places=3)

    def test_saturated_link_admits_nothing(self):
        # A/D = 1.0: no upshift is affordable, and the bisection floor is exactly 1.0.
        segments = [(1000, 2000)] * 25
        _, ratio = tb.grade_climb(segments, window=20, k=1, duration_ms=2000)
        self.assertAlmostEqual(ratio, 1.0, places=3)

    def test_one_slow_observation_dominates_at_k_1(self):
        # k=1 is the MAX over the window, so a single outlier collapses the admitted ratio. This
        # is why k is the robustness axis and is reported rather than fixed.
        segments = [(1000, 500)] * 24 + [(1000, 1900)]
        window = [(1000, 500)] * 19 + [(1000, 1900)]
        self.assertLess(
            tb.admissible_ratio(window, 1000, k=1, duration_ms=2000),
            tb.admissible_ratio(window, 1000, k=2, duration_ms=2000),
        )
        self.assertTrue(segments)

    def test_no_history_returns_none(self):
        self.assertIsNone(tb.grade_climb([(1, 1)], window=20, k=1, duration_ms=2000))


class AgainstTheCommittedCorpus(unittest.TestCase):
    """The three headline claims, pinned against the logs in the repository."""

    @classmethod
    def setUpClass(cls):
        root = pathlib.Path(__file__).resolve().parents[1]
        cls.cases = tb.cases(str(root / "docs/measurements/p1b-logs/*.log"))

    def test_corpus_is_present(self):
        self.assertGreaterEqual(len(self.cases), 10)

    def test_single_observation_bound_is_refuted(self):
        total = violations = 0
        for _, segments in self.cases:
            sub_total, sub_violations, _ = tb.grade_pairs(segments)
            total += sub_total
            violations += sub_violations
        # The claim recorded in the specification is "~37%". Pin it as clearly-refuted rather than
        # as a value echo: anything above a few percent kills the single-observation form.
        self.assertGreater(violations / total, 0.20)

    def test_order_statistic_is_conservative_against_nominal(self):
        for window, k in [(20, 1), (29, 3)]:
            total = exceed = 0
            for _, segments in self.cases:
                sub_total, sub_exceed, _ = tb.grade_order(segments, window, k)
                total += sub_total
                exceed += sub_exceed
            with self.subTest(window=window, k=k):
                self.assertGreater(total, 100)
                self.assertLessEqual(exceed / total, k / (window + 1))


if __name__ == "__main__":
    unittest.main(verbosity=2)
