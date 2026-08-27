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
import statistics
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


class TheCaptureChronology(unittest.TestCase):
    """The order captures are preferred in cannot be derived from their names, so it is stated."""

    def test_every_capture_on_disk_is_placed_in_the_chronology(self):
        """An unplaced capture must fail loudly. `sorted(reverse=True)` was the rule until a
        `j3b-logs` capture landed and reverse-alphabetical silently put every `p*` ahead of it —
        the stale-table failure this file exists to prevent, recurring in the mechanism meant to
        prevent it."""
        cp.captures_newest_first()  # raises SystemExit naming anything unlisted

    def test_the_stated_order_is_not_the_alphabetical_one(self):
        """If these ever coincide the bug is invisible again, and the explicit list looks like
        redundant ceremony to the next reader. They do not coincide today: `p2-logs` sorts above
        `j3b-logs` and is older."""
        import glob
        present = [pathlib.Path(d).name for d in glob.glob(str(ROOT / "docs/measurements/*-logs"))]
        stated = [n for n in cp.CAPTURE_ORDER if n in present]
        self.assertNotEqual(stated, sorted(present, reverse=True),
                            "stated and alphabetical order agree; this guard proves nothing")

    def test_the_table_reports_which_capture_each_rung_came_from(self):
        """A rung is never averaged across captures, but a TABLE routinely draws from two or three
        — and from a partial one while a capture is being taken. `provenance` is the only thing
        that makes that visible, so it is asserted rather than assumed."""
        points = cp.operating_points(cp.DEFAULT_FIXTURES)
        self.assertTrue(points)
        for rung, p in points.items():
            with self.subTest(rung=rung):
                self.assertIn(p["source"], cp.CAPTURE_ORDER)


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

    def test_a_sample_with_no_readable_reserve_is_skipped_rather_than_censused_as_zero(self):
        """`buf=none` is the app saying the playable reserve is not knowable this segment. It
        feeds `buf_median_ms`, the plant's starting reserve, so coercing it to 0 would drag that
        median down by exactly the count of segments whose audio lane happened to be quiet — and
        `sim.rs` would then model a television that starts every rung emptier than it does."""
        import tempfile
        line = ("abr: sample current=4000kbps media=4000kbps net=8000kbps buf={buf} "
                "vbuf=9000ms abuf=9000ms dur=2000ms prod=700pm n=1 decision=stay target=0kbps")
        with tempfile.TemporaryDirectory() as d:
            p = pathlib.Path(d) / "case.log"
            p.write_text("\n".join(line.format(buf=b) for b in ("9000ms", "none", "9000ms")) + "\n")
            rows = cp.pin_samples(p, 4000)
        self.assertEqual([r["buf"] for r in rows], [9000, 9000])

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
        committed corpus the `Down`/commit `decided` values are min 26, p50 749, p95 1 491 — and
        **max 36 164**. The six largest are 1 441, 1 451, 1 491, 1 502, 2 241, 36 164: a **24x jump
        from p95 and 16x from the second-largest value**, which is not a tail but a second regime.
        The record is a downshift whose warm-up fetch ran 36 156 ms on a collapsing link.

        **Restricted to records where `decided` MEANS the decision cost.** Before the leg split,
        `tx.finish("committed")` sat below the feed loop, so `decided` also contained the
        post-commit backpressure — a different quantity, and 17 of the 65 `Down` records are of
        that older kind. Pooling them is what the plan's own board caught in
        `docs/measurements/i2-transaction-cost.md` ("true upshift cost is 3 065 ms median, not
        9 563"), and the first version of this test repeated it: the pooled figures were p50 916 /
        p95 2 198, which UNDERSTATES the gap. ` prime=` is the marker, since that field arrived
        with the split.

        The cause of the outlier is structural: `candidate_prime_budget` and
        `candidate_warmup_budget` both opened with `if direction == Down { return None }`, so the
        fail-safe transaction had no deadline of any kind. 36 s is not a transaction cost, it is an
        unbounded transaction.

        This test pins the counter-example so it cannot quietly leave the corpus and let `H_ref`
        look derived again. **A landed deadline does not flip it** — the historical record stays in
        an append-only corpus — so the deadline's own evidence is graded by
        `NoTransactionOutspendsItsReserve` below instead. Delete this one only when `E_tx_down` has
        been RE-MEASURED with the deadline in place and `H_ref` derived from that.
        """
        rows = sorted(down_committed_ms())
        self.assertGreaterEqual(len(rows), 40, "too few records to say anything about the shape")
        p95 = rows[int(0.95 * (len(rows) - 1))]
        self.assertLess(statistics.median(rows), 2_000, "the ordinary regime is under a second")
        self.assertGreater(
            rows[-1], 10 * p95,
            f"the unbounded downshift is gone from the corpus (max {rows[-1]}ms vs p95 {p95}ms). "
            "If E_tx_down has been re-measured under the deadline, derive H_ref and delete this.")


def down_committed_ms():
    """`decided` for every `Down`/commit record whose `decided` means the DECISION cost.

    ` prime=` is the marker for the leg split; before it the field also carried the post-commit
    feed. Shared by two tests so the restriction cannot drift between them.
    """
    import glob
    pattern = re.compile(r"abr: tx Down \d+->\d+kbps outcome=committed decided=(\d+)ms")
    return [int(m.group(1))
            for path in sorted(glob.glob(str(ROOT / "docs/measurements/*-logs/*.log")))
            for line in pathlib.Path(path).read_text(errors="replace").splitlines()
            if " prime=" in line
            for m in [pattern.search(line)] if m]


class NoTransactionOutspendsItsReserve(unittest.TestCase):
    """**The property J3b enforces, graded on the corpus rather than asserted about the code.**

    A candidate transaction runs inline on the demux worker and stages its output until commit, so
    the playable reserve falls one millisecond per millisecond of wall clock while it runs. A
    transaction whose media fetches alone exceed the reserve it started with has therefore stalled
    playback, whatever it went on to decide.

    `warmup + graded` is a LOWER bound on the wall clock (the control plane sits on top of it), so
    this is the permissive form of the deadline — which is the point: it is the strongest statement
    a captured log can support, and it is already sharp enough to catch exactly one record.

    **One record in the whole corpus violates it, and it is the one the deadline exists to
    prevent.** It is grandfathered by name below because the corpus is append-only. Everything
    captured since is graded automatically; a second violation fails this test.
    """

    #: (capture directory, `decided` ms) — captured BEFORE `candidate_warmup_budget` bounded a
    #: downshift. Not a tolerance: an exhaustive list of the evidence that motivated the change.
    #: It shrinks to nothing when these captures are eventually pruned, and it must never grow.
    GRANDFATHERED = {("j3a-window-logs", 36156)}

    # `.*?` between `graded=` and `buf_start=` because `warmup_dl=` landed between them and the
    # corpus holds captures from both sides of that change. This grader must read BOTH: a
    # regex pinned to one field order silently drops half the evidence it exists to check.
    LEGS = re.compile(r"abr: tx (\w+) \d+->\d+kbps outcome=(\S+).*?warmup=(\d+|none)ms "
                      r"graded=(\d+|none)ms.*? buf_start=(-?\d+)ms")

    @classmethod
    def violations(cls):
        import glob
        out, total = [], 0
        for path in sorted(glob.glob(str(ROOT / "docs/measurements/*-logs/*.log"))):
            capture = pathlib.Path(path).parent.name
            for n, line in enumerate(pathlib.Path(path).read_text(errors="replace").splitlines(), 1):
                m = cls.LEGS.search(line)
                if not m:
                    continue
                total += 1
                spent = sum(int(g) for g in (m.group(3), m.group(4)) if g != "none")
                if spent > int(m.group(5)):
                    out.append((capture, n, m.group(1), spent, int(m.group(5))))
        return out, total

    def test_the_corpus_is_large_enough_to_mean_something(self):
        _, total = self.violations()
        self.assertGreaterEqual(total, 100, f"only {total} transactions carry a leg breakdown")

    def test_no_transaction_outside_the_named_ones_outspends_its_reserve(self):
        violations, total = self.violations()
        unexpected = [v for v in violations if (v[0], v[3]) not in self.GRANDFATHERED]
        self.assertFalse(
            unexpected,
            f"{len(unexpected)} of {total} transactions spent more than the reserve they started "
            f"with, and are not the pre-deadline record: {unexpected}")

    def test_the_grandfathered_record_is_still_there_so_this_test_can_still_fail(self):
        """A guard that only ever passes has stopped being a guard. If the pre-deadline capture is
        pruned, delete the entry rather than leaving a check for evidence that is gone."""
        found = {(v[0], v[3]) for v in self.violations()[0]}
        self.assertEqual(
            found & self.GRANDFATHERED, self.GRANDFATHERED,
            "the grandfathered pre-deadline record is no longer in the corpus; drop it from the set")


if __name__ == "__main__":
    unittest.main(verbosity=2)
