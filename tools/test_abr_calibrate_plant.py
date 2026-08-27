#!/usr/bin/env python3
"""Tests for `tools/abr-calibrate-plant.py`, and the gate that stops `sim.rs` going stale again.

**The gate is the point of this file.** `rust-modules/src/abr/sim.rs` is the closed-loop plant the
ABR controller is graded against, and its operating points were three numbers somebody typed. The
fixture pack was rebuilt underneath them — at rung 720 the delivered rate moved 1381 -> 806 kbps
(1.72x), at rung 4000 the other way — and nothing failed, because nothing recomputes a constant.
The plant went on modelling a television that no longer existed at two of its three points, and the
test that was supposed to catch exactly that passed, because the prediction AND the observation had
been hand-copied out of the same superseded document and went stale together.

So both halves are generated now, and `TheShippedTableMatchesTheEvidence` below re-derives them from
the committed logs and fails if `sim.rs` disagrees. That is the only mechanism in this repository
that connects the plant to the evidence it claims to come from.
"""

from __future__ import annotations

import importlib.util
import pathlib
import re
import unittest

_spec = importlib.util.spec_from_file_location(
    "abr_calibrate_plant", pathlib.Path(__file__).with_name("abr-calibrate-plant.py")
)
assert _spec and _spec.loader
cp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cp)

ROOT = pathlib.Path(__file__).resolve().parents[1]
SIM_RS = ROOT / "rust-modules" / "src" / "abr" / "sim.rs"


def rust_arms(source: str, fn: str) -> dict[int, tuple]:
    """The `<key> => (<values>),` arms of one `match` in `sim.rs`, keyed by rung.

    Parsed rather than imported because there is no way to call Rust from here, and hard-coding the
    expected table would recreate the exact duplication this gate exists to remove.
    """
    start = source.index(f"fn {fn}(")
    body = source[start:source.index("\n    }", start)]
    out = {}
    # `\d[\d_]*` and not `[\d_]+`: the latter also matches the wildcard `_ => return None` arm,
    # whose key is empty once the underscores are stripped.
    for key, vals in re.findall(r"^\s*(\d[\d_]*)\s*=>\s*\(?([^)\n]*?)\)?,\s*$", body, re.M):
        parts = [v.strip() for v in vals.split(",") if v.strip()]
        # Drop Rust's digit separators and the type suffix the FIRST arm carries (`383u32`), in
        # that order. Stripping all non-digits instead turns `383u32` into 38332.
        nums = tuple(
            int(re.sub(r"(?:u|i)(?:8|16|32|64|128|size)$", "", p.replace("_", "")))
            for p in parts if re.search(r"\d", p)
        )
        out[int(key.replace("_", ""))] = nums
    return out


class TheFixtureMapIsReadNotCopied(unittest.TestCase):
    """A rung -> clip mapping copied into this tool could point at the wrong clip forever."""

    def test_every_rung_maps_to_a_fixture(self):
        fixture = cp.fixture_map()
        self.assertGreaterEqual(len(fixture), 12)
        for rung, name in fixture.items():
            self.assertTrue(rung.isdigit(), rung)
            self.assertTrue(name.endswith(".ts"), name)

    def test_it_comes_from_the_server_that_actually_serves_them(self):
        text = (ROOT / "tests" / "serve_fixtures.py").read_text()
        for rung, name in cp.fixture_map().items():
            self.assertIn(f'"{rung}": "{name}"', text.replace("\n", " ").replace("  ", " ")
                          if f'"{rung}": "{name}"' not in text else text)


class Settling(unittest.TestCase):
    """The census convention: the first quarter of a pin is queue fill-in, not the ceiling."""

    def test_the_first_quarter_is_dropped(self):
        rows = list(range(100))
        self.assertEqual(cp.settled(rows), list(range(25, 100)))

    def test_a_short_run_is_kept_whole_rather_than_reduced_to_nothing(self):
        self.assertEqual(cp.settled([1, 2, 3]), [1, 2, 3])


class PinSamples(unittest.TestCase):
    def test_only_samples_taken_ON_the_pinned_rung_count(self):
        """`p1`'s pin_4000 never reached 4000 — it sat at 6000 for the whole run. A parser that
        ignored `current=` would have calibrated rung 4000 from rung 6000's segments, which is
        very close to what the stale table actually did."""
        import tempfile
        line = ("abr: sample current={cur}kbps media=4386kbps net=100000kbps buf=18000ms "
                "vbuf=18000ms abuf=18000ms dur=2000ms prod=200pm n=9 decision=stay target=0kbps")
        with tempfile.TemporaryDirectory() as d:
            p = pathlib.Path(d) / "case.log"
            p.write_text("\n".join(line.format(cur=c) for c in (6000, 4000, 6000)) + "\n")
            rows = cp.pin_samples(p, 4000)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["media"], 4386)

    def test_overhead_is_acquisition_minus_the_implied_transfer(self):
        import tempfile
        # media 4000 kbps over dur 2000 ms at net 8000 kbps -> active 1000 ms.
        # prod 700 pm of 2000 ms -> acquisition 1400 ms. Overhead 400 ms.
        line = ("abr: sample current=4000kbps media=4000kbps net=8000kbps buf=1ms "
                "vbuf=1ms abuf=1ms dur=2000ms prod=700pm n=1 decision=stay target=0kbps")
        with tempfile.TemporaryDirectory() as d:
            p = pathlib.Path(d) / "case.log"
            p.write_text(line + "\n")
            rows = cp.pin_samples(p, 4000)
        self.assertAlmostEqual(rows[0]["overhead"], 400.0)

    def test_overhead_never_goes_negative(self):
        import tempfile
        line = ("abr: sample current=4000kbps media=4000kbps net=1000kbps buf=1ms "
                "vbuf=1ms abuf=1ms dur=2000ms prod=100pm n=1 decision=stay target=0kbps")
        with tempfile.TemporaryDirectory() as d:
            p = pathlib.Path(d) / "case.log"
            p.write_text(line + "\n")
            rows = cp.pin_samples(p, 4000)
        self.assertGreaterEqual(rows[0]["overhead"], 0.0)


class TheShippedTableMatchesTheEvidence(unittest.TestCase):
    """**The gate.** `sim.rs`'s table against a fresh derivation from the committed logs."""

    @classmethod
    def setUpClass(cls):
        cls.source = SIM_RS.read_text()
        cls.points = cp.operating_points(cp.DEFAULT_FIXTURES)

    def test_the_derivation_produced_something(self):
        self.assertGreaterEqual(len(self.points), 7, "the committed census should cover 7 rungs")

    def test_every_shipped_operating_point_is_the_derived_one(self):
        shipped = rust_arms(self.source, "point")
        for rung, arm in sorted(shipped.items()):
            with self.subTest(rung=rung):
                self.assertIn(rung, self.points, f"rung {rung} is in sim.rs but not in the census")
                p = self.points[rung]
                self.assertEqual(
                    arm, (p["ts_kbps"], p["audio_es_kbps"], p["overhead_ms"]),
                    f"rung {rung}: sim.rs disagrees with the committed logs. Regenerate with "
                    "`tools/abr-calibrate-plant.py --rust`.")

    def test_every_censused_rung_is_shipped(self):
        """The other direction: a rung measured and then left out of the plant is coverage the
        simulator silently does not have, and `sim.rs` REFUSES an uncalibrated rung mid-run."""
        shipped = set(rust_arms(self.source, "point"))
        missing = sorted(set(self.points) - shipped)
        self.assertFalse(missing, f"censused but not calibrated: {missing}")

    def test_the_census_reserve_table_matches_too(self):
        shipped = rust_arms(self.source, "census_buf_ms")
        for rung, arm in sorted(shipped.items()):
            with self.subTest(rung=rung):
                self.assertEqual(arm, (self.points[rung]["buf_median_ms"],),
                                 f"rung {rung}: sim.rs's census median is not the logs'.")

    def test_the_calibrated_constant_lists_the_same_rungs(self):
        m = re.search(r"CALIBRATED:\s*\[u32;\s*(\d+)\]\s*=\s*\[([^\]]*)\]", self.source)
        self.assertIsNotNone(m, "CALIBRATED not found in sim.rs")
        listed = [int(x.strip().replace("_", "")) for x in m.group(2).split(",") if x.strip()]
        self.assertEqual(int(m.group(1)), len(listed), "the array length must match its contents")
        self.assertEqual(sorted(listed), sorted(rust_arms(self.source, "point")))


class TheTransactionLegs(unittest.TestCase):
    """Three of four are measured. The fourth is absent for a STRUCTURAL reason, not an oversight."""

    @classmethod
    def setUpClass(cls):
        cls.legs = {k: cp.leg_summary(v) for k, v in cp.transaction_legs().items()}

    def test_three_legs_are_measured(self):
        for key in (("Up", True), ("Up", False), ("Down", True)):
            with self.subTest(leg=key):
                self.assertIn(key, self.legs)
                self.assertGreater(self.legs[key]["n"], 0)

    def test_a_downshift_reject_has_never_been_observed(self):
        """`Controller::candidate_ready` returns `true` for every downshift that produced a
        decodable segment and one segment of reserve, so a down-reject needs a decode or raster
        failure to happen at all. `sim.rs` must keep refusing rather than inventing it — a
        fabricated leg is how the previous plant made `T_down` growing on a collapsing link
        unrepresentable."""
        self.assertNotIn(("Down", False), self.legs)

    def test_an_upshift_costs_more_than_a_downshift(self):
        """Structural: an upshift fetches a warm-up AND a graded segment, a downshift only a
        warm-up. If this ever inverts, the legs have been mis-attributed."""
        up = self.legs[("Up", True)]
        down = self.legs[("Down", True)]
        self.assertGreater(up["warmup_acq_ms"] + up["graded_acq_ms"],
                           down["warmup_acq_ms"] + down["graded_acq_ms"])

    def test_a_downshift_has_no_graded_segment(self):
        self.assertEqual(self.legs[("Down", True)]["graded_acq_ms"], 0)

    def test_the_downshift_cost_is_bimodal_because_it_has_no_deadline(self):
        """**`E_tx_down` is not one quantity, and the specification's `H_ref` assumes it is.**

        §7a derives `H_ref = E_tx_down + D = 3 424 ms` from a single 1 424 ms observation. Over the
        whole committed corpus the `Down/commit` `decided` values are min 26, p50 916, p95 2 198 —
        and **max 36 164**. A 16x jump from p95 with nothing in between is not a tail, it is a
        second regime, and the record is a downshift whose warm-up fetch ran 36 156 ms on a
        collapsing link.

        The cause is structural: `candidate_prime_budget` and `candidate_warmup_budget` both open
        with `if direction == Down { return None }`, so the fail-safe transaction has no deadline of
        any kind. 36 s is not a transaction cost, it is an unbounded transaction.

        This test exists so the counter-example cannot quietly leave the corpus and let `H_ref` look
        derived again. It fails if the spread collapses — at which point either a deadline landed
        (delete this and derive `H_ref` from it) or the evidence was dropped.
        """
        # `transaction_legs` keeps the three legs, not `decided`, so read it here rather than
        # widen that API for one test.
        import glob
        import re as _re
        import statistics
        pattern = _re.compile(r"abr: tx Down \d+->\d+kbps outcome=committed decided=(\d+)ms")
        rows = [int(m.group(1))
                for p in sorted(glob.glob(str(ROOT / "docs/measurements/*-logs/*.log")))
                for m in pattern.finditer(pathlib.Path(p).read_text(errors="replace"))]
        self.assertGreaterEqual(len(rows), 40, "too few records to say anything about the shape")
        rows.sort()
        p95 = rows[int(0.95 * (len(rows) - 1))]
        self.assertLess(statistics.median(rows), 2_000, "the ordinary regime is around a second")
        self.assertGreater(
            rows[-1], 10 * p95,
            f"the unbounded downshift is gone from the corpus (max {rows[-1]}ms vs p95 {p95}ms). "
            "If a downshift deadline landed, derive H_ref from it and delete this test.")


if __name__ == "__main__":
    unittest.main(verbosity=2)
