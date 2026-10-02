#!/usr/bin/env python3
"""ci/check-build-budgets.py against canned inputs: pass, fail, warn, and the json schema.

Hermetic: no cargo, no ARM toolchain. The `cargo metadata` documents are hand-built (a root, a
workspace member, registry crates, a duplicated crate, a dev-only crate and a build-only crate),
the "binary" is a few bytes behind an ELF magic, and the source tree is a temp directory.
"""
from __future__ import annotations

import contextlib
import copy
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("budgets", ROOT / "ci" / "check-build-budgets.py")
b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b)

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


def metadata(extra_normal=(), dup=True):
    """root -> {member (workspace), serde, syn 1 + syn 2 (via two proc-macros), cc (build)}; dev -> rcgen."""
    pkgs = {
        "root": None, "member": None,
        "serde": REGISTRY, "syn1": REGISTRY, "syn2": REGISTRY, "macro-a": REGISTRY, "macro-b": REGISTRY,
        "cc": REGISTRY, "rcgen": REGISTRY,
    }
    for name in extra_normal:
        pkgs[name] = REGISTRY

    def pkg(i):
        return {"id": i, "name": "syn" if i.startswith("syn") else i, "version": "2.0.0" if i == "syn2" else "1.0.0",
                "source": pkgs[i]}

    def dep(i, kind=None):
        return {"pkg": i, "dep_kinds": [{"kind": kind, "target": None}]}

    deps = {
        "root": [dep("member"), dep("serde"), dep("macro-a"), dep("cc", "build"), dep("rcgen", "dev")]
                + [dep(n) for n in extra_normal],
        "macro-a": [dep("syn1")],
        "serde": [dep("macro-b")],
        "macro-b": [dep("syn2")] if dup else [dep("syn1")],
    }
    return {
        "packages": [pkg(i) for i in pkgs],
        "resolve": {"root": "root", "nodes": [{"id": i, "deps": deps.get(i, [])} for i in pkgs]},
    }


def budgets_doc():
    def entry(limit, measured, mode="fail", unit="packages"):
        return {"what": "w", "unit": unit, "mode": mode, "limit": limit, "measured": measured,
                "measured_on": "2026-10-02", "headroom": "+3"}

    return {
        "schema": 1,
        "how_to_raise": "edit the json",
        "budgets": {
            "binary_bytes": entry(1000, 900, unit="bytes"),
            "packages_default": entry(10, 7),
            "packages_no_default_features": entry(10, 7),
            "duplicate_versions_default": entry(2, 1, unit="crate names"),
            "duplicate_versions_no_default_features": entry(2, 1, unit="crate names"),
            "source_lines": entry(5, 4, mode="warn", unit="lines"),
        },
    }


class Fixture:
    def __init__(self, test, doc=None, meta=None, binary_size=900, lines=3):
        self.dir = tempfile.TemporaryDirectory()
        test.addCleanup(self.dir.cleanup)
        d = Path(self.dir.name)
        self.budgets = d / "budgets.json"
        self.budgets.write_text(json.dumps(doc or budgets_doc()))
        self.meta = d / "meta.json"
        self.meta.write_text(json.dumps(meta or metadata()))
        self.binary = d / "plxnative"
        self.binary.write_bytes(b"\x7fELF" + b"\0" * (binary_size - 4))
        self.src = d / "src"
        (self.src / "sub").mkdir(parents=True)
        (self.src / "a.rs").write_text("fn a() {}\n" * lines)
        (self.src / "sub" / "b.rs").write_text("")
        (self.src / "ignored.txt").write_text("x\n" * 99)

    def run(self, *extra):
        out, err = io.StringIO(), io.StringIO()
        argv = ["--budgets", str(self.budgets), "--binary", str(self.binary),
                "--metadata-default", str(self.meta), "--metadata-no-default", str(self.meta),
                "--src", str(self.src), *extra]
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = b.main(argv)
        return code, out.getvalue(), err.getvalue()


class GraphMetrics(unittest.TestCase):
    def test_counts_third_party_normal_and_build_but_not_dev_or_workspace(self):
        m = b.graph_metrics(metadata())
        # serde, macro-a, macro-b, syn1, syn2, cc(build) -- not rcgen (dev) and not member/root
        self.assertEqual(m["packages"], 6)

    def test_duplicates_are_crate_names_with_two_versions(self):
        self.assertEqual(b.graph_metrics(metadata(dup=True))["duplicates"], 1)
        self.assertEqual(b.graph_metrics(metadata(dup=False))["duplicates"], 0)

    def test_a_dependency_changes_the_count(self):
        self.assertEqual(b.graph_metrics(metadata(extra_normal=("extra",)))["packages"], 7)

    def test_unresolved_metadata_is_rejected(self):
        with self.assertRaises(b.BudgetError):
            b.graph_metrics({"packages": [], "resolve": None})


class Verdicts(unittest.TestCase):
    def test_everything_within_budget_passes(self):
        code, out, err = Fixture(self).run()
        self.assertEqual(code, 0, out + err)
        self.assertIn("binary_bytes", out)
        self.assertNotIn("FAIL", out)

    def test_binary_over_budget_fails_with_the_raise_instruction(self):
        code, out, _ = Fixture(self, binary_size=1001).run()
        self.assertEqual(code, 1)
        self.assertIn("::error::build budget binary_bytes exceeded: 1,001 bytes > limit 1,000", out)
        self.assertIn("edit ci/build-budgets.json in the SAME pull request", out)
        self.assertIn("justify the growth in the PR body", out)

    def test_exactly_at_the_limit_passes(self):
        self.assertEqual(Fixture(self, binary_size=1000).run()[0], 0)

    def test_package_count_over_budget_fails(self):
        doc = budgets_doc()
        doc["budgets"]["packages_default"]["limit"] = 5
        doc["budgets"]["packages_default"]["measured"] = 5
        code, out, _ = Fixture(self, doc=doc).run()
        self.assertEqual(code, 1)
        self.assertIn("packages_default exceeded: 6 packages > limit 5", out)

    def test_duplicate_versions_over_budget_fail(self):
        doc = budgets_doc()
        doc["budgets"]["duplicate_versions_no_default_features"]["limit"] = 0
        doc["budgets"]["duplicate_versions_no_default_features"]["measured"] = 0
        code, out, _ = Fixture(self, doc=doc).run()
        self.assertEqual(code, 1)
        self.assertIn("duplicate_versions_no_default_features exceeded", out)

    def test_source_lines_over_budget_only_warns(self):
        code, out, _ = Fixture(self, lines=50).run()
        self.assertEqual(code, 0)
        self.assertIn("::warning::build budget source_lines exceeded: 50 lines > limit 5", out)
        self.assertNotIn("::error::", out)

    def test_only_rust_files_are_counted(self):
        self.assertEqual(b.source_lines(Fixture(self, lines=7).src), 7)

    def test_a_warn_does_not_hide_a_fail(self):
        code, out, _ = Fixture(self, binary_size=2000, lines=50).run()
        self.assertEqual(code, 1)
        self.assertIn("::error::", out)
        self.assertIn("::warning::", out)

    def test_only_the_requested_measurements_are_graded(self):
        fx = Fixture(self)
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = b.main(["--budgets", str(fx.budgets), "--binary", str(fx.binary)])
        self.assertEqual(code, 0)
        self.assertIn("binary_bytes", out.getvalue())
        self.assertNotIn("packages_default", out.getvalue())

    def test_missing_or_non_elf_binary_is_unusable(self):
        fx = Fixture(self)
        fx.binary.write_bytes(b"not an elf")
        self.assertEqual(fx.run()[0], 2)
        fx.binary.unlink()
        self.assertEqual(fx.run()[0], 2)

    def test_nothing_to_measure_is_an_error(self):
        out = io.StringIO()
        with contextlib.redirect_stderr(out):
            self.assertEqual(b.main(["--budgets", str(Fixture(self).budgets)]), 2)


class Schema(unittest.TestCase):
    def test_the_real_budgets_file_is_valid(self):
        doc = json.loads((ROOT / "ci" / "build-budgets.json").read_text())
        self.assertEqual(b.schema_problems(doc), [])
        self.assertEqual(sorted(doc["budgets"]), sorted(b.KNOWN))
        self.assertIn("SAME pull request", doc["how_to_raise"])

    def test_real_budgets_carry_headroom_over_what_they_were_measured_at(self):
        for name, e in json.loads((ROOT / "ci" / "build-budgets.json").read_text())["budgets"].items():
            self.assertGreaterEqual(e["limit"], e["measured"], name)
            self.assertRegex(e["measured_on"], r"^\d{4}-\d{2}-\d{2}$", name)

    def test_each_defect_is_named(self):
        cases = {
            "schema": lambda d: d.__setitem__("schema", 2),
            "how_to_raise": lambda d: d.pop("how_to_raise"),
            "missing": lambda d: d["budgets"].pop("binary_bytes"),
            "unknown": lambda d: d["budgets"].__setitem__("surprise", {}),
            "field": lambda d: d["budgets"]["packages_default"].pop("measured_on"),
            "mode": lambda d: d["budgets"]["packages_default"].__setitem__("mode", "maybe"),
            "type": lambda d: d["budgets"]["packages_default"].__setitem__("limit", "10"),
            "below": lambda d: d["budgets"]["packages_default"].__setitem__("limit", 1),
        }
        self.assertEqual(b.schema_problems(budgets_doc()), [])
        for label, mutate in cases.items():
            doc = copy.deepcopy(budgets_doc())
            mutate(doc)
            self.assertTrue(b.schema_problems(doc), label)

    def test_an_invalid_file_is_exit_2(self):
        doc = budgets_doc()
        doc["schema"] = 9
        self.assertEqual(Fixture(self, doc=doc).run()[0], 2)


if __name__ == "__main__":
    unittest.main()
