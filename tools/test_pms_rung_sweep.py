#!/usr/bin/env python3
"""Offline tests for pms-rung-sweep.py. No PMS, no network, no device.

Everything here is about ONE failure mode: a table that looks right and is not. The sweep's
output is a byte RATIO between two rungs, and every way that ratio can silently stop meaning
"the actuator's effect" is a case below -- unpaired indices, unequal media spans, an assumed
segment duration, and an arithmetic mean over ratios.
"""

import importlib.util
import sys
import unittest
from pathlib import Path


TOOL = Path(__file__).with_name("pms-rung-sweep.py")
SPEC = importlib.util.spec_from_file_location("pms_rung_sweep", TOOL)
sweep = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = sweep
SPEC.loader.exec_module(sweep)


def report(bandwidth_bps, sizes, duration=2.0, first_index=0, ttfb=200.0, body=5.0,
           decided=None, raster="1280x720"):
    """A probe report with the fields the sweep reads, and nothing else."""
    return {
        "start": {
            "timing": {"total_ms": 10.0},
            "variant_attributes": [{"BANDWIDTH": str(bandwidth_bps), "RESOLUTION": raster}],
        },
        "decision": {"timing": {"total_ms": 17.0}, "summary": {"bitrate": decided}},
        "variants": [{"timing": {"total_ms": 89.0}}],
        "media": {"child_count": 4235},
        "segments": [
            {
                "index": first_index + offset,
                "status": 200,
                "timing": {"bytes": size, "ttfb_ms": ttfb, "body_ms": body},
                "probe": {"format": {"duration": str(
                    duration[offset] if isinstance(duration, list) else duration
                )}},
            }
            for offset, size in enumerate(sizes)
        ],
    }


class SegmentRows(unittest.TestCase):
    def test_a_failed_segment_is_dropped_rather_than_counted_as_zero_bytes(self):
        one = report(1_000_000, [1000, 2000])
        one["segments"][1]["status"] = 503
        rows = sweep.segment_rows(one)
        self.assertEqual([row["index"] for row in rows], [0])

    def test_an_unprobeable_duration_becomes_none_and_not_a_default_two_seconds(self):
        # An assumed 2.0 s would turn a missing measurement into a confident wrong bitrate.
        one = report(1_000_000, [1000])
        one["segments"][0]["probe"] = {"format": {}}
        self.assertIsNone(sweep.segment_rows(one)[0]["duration_s"])
        self.assertIsNone(sweep.segment_rate_bps(sweep.segment_rows(one)[0]))

    def test_rate_uses_the_measured_duration_not_the_playlist_integer(self):
        rows = sweep.segment_rows(report(1_000_000, [250_000], duration=2.5))
        self.assertAlmostEqual(sweep.segment_rate_bps(rows[0]), 250_000 * 8 / 2.5)


class Pairing(unittest.TestCase):
    def test_only_indices_present_on_both_sides_are_paired(self):
        left = sweep.segment_rows(report(1_000_000, [1, 2, 3], first_index=0))
        right = sweep.segment_rows(report(2_000_000, [1, 2, 3], first_index=2))
        indices, _, _ = sweep.pairable(left, right)
        self.assertEqual(indices, [2])

    def test_equal_indices_with_unequal_media_spans_are_reported_as_a_mismatch(self):
        # The trap this exists for: two rungs segmented differently still share index numbers,
        # so index equality alone would happily divide one scene's bytes by another's.
        left_rows = sweep.segment_rows(report(1_000_000, [100, 100], duration=[2.0, 2.0]))
        right_rows = sweep.segment_rows(report(2_000_000, [100, 100], duration=[2.0, 4.0]))
        indices, left, right = sweep.pairable(left_rows, right_rows)
        self.assertEqual(sweep.duration_mismatch(indices, left, right), [1])

    def test_a_frame_of_difference_is_not_a_mismatch(self):
        # 24 fps is 42 ms; two encodes of one span legitimately differ by a frame or two.
        left_rows = sweep.segment_rows(report(1_000_000, [100], duration=[2.000]))
        right_rows = sweep.segment_rows(report(2_000_000, [100], duration=[2.042]))
        indices, left, right = sweep.pairable(left_rows, right_rows)
        self.assertEqual(sweep.duration_mismatch(indices, left, right), [])

    def test_mismatched_indices_are_excluded_from_the_reported_ratio(self):
        reports = {
            1000: report(1_000_000, [100, 100], duration=[2.0, 2.0]),
            2000: report(2_000_000, [400, 400], duration=[2.0, 4.0]),
        }
        pair = sweep.analyse(reports)["pairs"][0]
        self.assertEqual(pair["paired"], 1)
        self.assertEqual(pair["dropped_for_duration"], 1)
        # Only index 0 survives: 400 bytes over 2 s against 100 over 2 s is exactly 4x.
        self.assertAlmostEqual(pair["delivered_ratio"]["geomean"], 4.0)


class Ratios(unittest.TestCase):
    def test_ratios_average_geometrically_so_a_halving_and_a_doubling_cancel(self):
        stats = sweep.ratio_stats([0.5, 2.0])
        self.assertAlmostEqual(stats["geomean"], 1.0)
        # The arithmetic mean of the same pair is 1.25 -- a 25% bias toward upshifting.
        self.assertNotAlmostEqual(stats["geomean"], 1.25)

    def test_empty_and_non_positive_inputs_return_none_rather_than_zero(self):
        self.assertIsNone(sweep.ratio_stats([]))
        self.assertIsNone(sweep.ratio_stats([0, -1]))

    def test_spread_is_max_over_min(self):
        self.assertAlmostEqual(sweep.ratio_stats([2.0, 4.0, 8.0])["spread"], 4.0)


class Analysis(unittest.TestCase):
    def test_s_is_delivered_over_declared_and_is_dimensionless(self):
        # 250 000 bytes over 2 s is exactly 1 Mbit/s; declared 2 Mbit/s gives s = 0.5.
        rungs = sweep.analyse({4000: report(2_000_000, [250_000])})["rungs"]
        self.assertAlmostEqual(rungs[0]["s"]["median"], 0.5)

    def test_a_rung_with_no_declared_bandwidth_reports_no_s_rather_than_dividing(self):
        one = report(1_000_000, [1000])
        one["start"]["variant_attributes"] = []
        rungs = sweep.analyse({4000: one})["rungs"]
        self.assertIsNone(rungs[0]["declared_kbps"])
        self.assertIsNone(rungs[0]["s"])

    def test_catalog_error_is_the_ladders_assumption_against_what_arrived(self):
        # Catalog says 4000/2000 = 2.0x. The server actually delivers 1.0x -- the ladder step
        # buys nothing. That gap is the whole finding this tool exists to surface.
        reports = {
            2000: report(2_000_000, [250_000, 250_000]),
            4000: report(4_000_000, [250_000, 250_000]),
        }
        pair = sweep.analyse(reports)["pairs"][0]
        self.assertAlmostEqual(pair["catalog_ratio"], 2.0)
        self.assertAlmostEqual(pair["declared_ratio"], 2.0)
        self.assertAlmostEqual(pair["delivered_ratio"]["geomean"], 1.0)

    def test_pairs_are_adjacent_in_rung_order_not_report_insertion_order(self):
        reports = {
            4000: report(4_000_000, [100]),
            320: report(320_000, [100]),
            2000: report(2_000_000, [100]),
        }
        pairs = sweep.analyse(reports)["pairs"]
        self.assertEqual(
            [(pair["from_kbps"], pair["to_kbps"]) for pair in pairs], [(320, 2000), (2000, 4000)]
        )


class Rendering(unittest.TestCase):
    def test_every_table_renders_with_missing_values_instead_of_raising(self):
        one = report(1_000_000, [1000])
        one["start"]["variant_attributes"] = []
        one["variants"] = []
        one["decision"]["timing"] = {}
        text = sweep.render(sweep.analyse({4000: one, 8000: report(2_000_000, [2000])}))
        self.assertIn("n/a", text)
        self.assertIn("| 4000 ", text)

    def test_ladder_rasters_are_the_shipped_pairing(self):
        self.assertEqual(sweep.rung_raster(320), "426x240")
        self.assertEqual(sweep.rung_raster(4000), "1280x720")
        self.assertEqual(sweep.rung_raster(22000), "3840x2160")

    def test_a_rate_off_the_ladder_still_resolves_to_a_raster(self):
        self.assertEqual(sweep.rung_raster(99000), "3840x2160")
        self.assertEqual(sweep.rung_raster(1), "426x240")


class SizeBounds(unittest.TestCase):
    """The two bounds a size-based admission rule could rest on, and their failure modes."""

    def test_b1_counts_a_segment_that_exceeds_its_own_declared_rate(self):
        # 500 000 bytes over 2 s is 2 Mbit/s against a declared 1 Mbit/s: s = 2.0, a violation.
        # If this ever fires on real data, the manifest is not a bound and B1 is unusable.
        rungs = sweep.analyse({4000: report(1_000_000, [250_000, 500_000])})["rungs"]
        self.assertEqual(sum(1 for v in rungs[0]["s"]["_values"] if v > 1.0), 1)

    def test_b2_reports_no_violation_when_delivery_stays_under_the_declared_ratio(self):
        # Declared doubles; delivered only 1.5x. The declared ratio bounds it.
        reports = {
            2000: report(2_000_000, [200_000, 200_000]),
            4000: report(4_000_000, [300_000, 300_000]),
        }
        pair = sweep.analyse(reports)["pairs"][0]
        self.assertEqual(pair["relative_bound_violations"], 0)
        self.assertAlmostEqual(pair["relative_bound_slack"]["median"], 0.75)

    def test_b2_counts_a_step_that_delivers_more_than_it_declared(self):
        # Declared doubles; delivered triples. The bound is broken, and an admission rule built
        # on it would have admitted a rung that then over-ran its own budget by 50%.
        reports = {
            2000: report(2_000_000, [200_000]),
            4000: report(4_000_000, [600_000]),
        }
        pair = sweep.analyse(reports)["pairs"][0]
        self.assertEqual(pair["relative_bound_violations"], 1)
        self.assertAlmostEqual(pair["relative_bound_slack"]["max"], 1.5)

    def test_b2_is_none_rather_than_zero_when_a_rung_declared_nothing(self):
        # A missing declaration must not read as "no violations found".
        blind = report(2_000_000, [200_000])
        blind["start"]["variant_attributes"] = []
        pair = sweep.analyse({2000: blind, 4000: report(4_000_000, [300_000])})["pairs"][0]
        self.assertIsNone(pair["relative_bound_violations"])
        self.assertIsNone(pair["relative_bound_slack"])

    def test_the_bound_table_names_both_bounds_and_survives_a_blind_rung(self):
        blind = report(2_000_000, [200_000])
        blind["start"]["variant_attributes"] = []
        text = sweep.render(sweep.analyse({2000: blind, 4000: report(4_000_000, [300_000])}))
        self.assertIn("B1 rate <= declared", text)
        self.assertNotIn("B2 ratio <= declared ratio | 2000->4000", text)


class CsvExport(unittest.TestCase):
    def test_every_sampled_segment_becomes_one_row_with_its_declared_rate(self):
        text = sweep.segments_csv({4000: report(3_775_000, [100, 200])})
        rows = text.strip().splitlines()
        self.assertEqual(rows[0], "request_kbps,declared_bps,index,bytes,duration_s,ttfb_ms,body_ms")
        self.assertEqual(rows[1], "4000,3775000,0,100,2.0,200.0,5.0")
        self.assertEqual(len(rows), 3)

    def test_a_rung_that_declared_nothing_leaves_the_field_empty_rather_than_zero(self):
        blind = report(1_000_000, [100])
        blind["start"]["variant_attributes"] = []
        self.assertIn("4000,,0,100,", sweep.segments_csv({4000: blind}))


if __name__ == "__main__":
    unittest.main(verbosity=2)
