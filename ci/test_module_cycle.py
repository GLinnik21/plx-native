#!/usr/bin/env python3
"""Tests for ci/check-module-cycle.py against a synthetic fixture crate.

The fixture is a four-module layering (gfx < ui < screens < app) plus an unrelated leaf `hls`,
written under a temp `rust-modules/src` so the reported `file:line` paths look like the real ones.
Each test mutates it the way a real change would and asserts the verdict and the evidence printed.
"""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("module_cycle", Path(__file__).with_name("check-module-cycle.py"))
checker = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = checker
SPEC.loader.exec_module(checker)

LIB = """\
mod app;
mod gfx;
#[cfg(feature = "hostsim")]
mod hls;
mod screens;
mod ui;
#[cfg(test)]
mod testonly;
"""

# The clean, layered crate: ui -> gfx, screens -> ui, app -> screens. No cycle.
FILES = {
    "lib.rs": LIB,
    "gfx.rs": "pub fn blit() {}\n",
    "hls.rs": "pub fn parse() {}\n",
    "ui.rs": "use crate::gfx;\npub fn draw() { gfx::blit(); }\n",
    "screens.rs": "use crate::ui;\npub fn page() { ui::draw(); }\n",
    "app.rs": "use crate::{screens, ui::draw};\npub fn run() { screens::page(); }\n",
    "testonly.rs": "use crate::app;\n",
}


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.src = self.root / "rust-modules" / "src"
        self.src.mkdir(parents=True)
        self.baseline = self.root / "baseline.json"
        for name, text in FILES.items():
            self.write(name, text)

    def write(self, rel, text):
        p = self.src / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(textwrap.dedent(text) if text.startswith("\n") else text)

    def append(self, rel, text):
        with (self.src / rel).open("a") as f:
            f.write(text)

    def run_check(self, *extra):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = checker.main(["--src", str(self.src), "--baseline", str(self.baseline), *extra])
        return code, out.getvalue(), err.getvalue()

    def update(self):
        code, _, _ = self.run_check("--update-baseline")
        self.assertEqual(code, 0)
        return json.loads(self.baseline.read_text())


class Layered(Fixture):
    def test_layered_crate_has_no_cycle_and_passes(self):
        snap = self.update()
        self.assertEqual(snap["members"], [])
        self.assertEqual(snap["size"], 0)
        code, out, err = self.run_check()
        self.assertEqual(code, 0, err)
        self.assertIn("ok", out)

    def test_upward_edge_pulling_a_module_into_a_cycle_fails_and_names_file_line(self):
        self.update()
        self.write("gfx.rs", "pub fn blit() {}\n\nfn up() { let _ = crate::ui::draw; }\n")
        code, out, err = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("'gfx'", err)
        self.assertIn("rust-modules/src/gfx.rs:3", err)
        self.assertIn("gfx -> ui", err)
        self.assertIn("ui -> gfx", err)  # the other half of the path, with its own file:line
        self.assertIn("rust-modules/src/ui.rs:1", err)

    def test_group_import_edge_is_found_with_its_own_line(self):
        self.update()
        self.write("hls.rs", "use crate::{\n    gfx,\n    app::run,\n};\n")
        self.write("app.rs", "use crate::{screens, hls::parse};\n")
        code, _, err = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("rust-modules/src/hls.rs:3", err)  # `app::run` sits on line 3 of the group
        self.assertIn("rust-modules/src/app.rs:1", err)

    def test_new_module_landing_inside_the_cycle_fails(self):
        self.update()
        self.write("lib.rs", LIB + "mod newmod;\n")
        self.write("newmod.rs", "use crate::app;\npub fn x() { crate::ui::draw(); }\n")
        self.write("app.rs", "use crate::{screens, newmod};\n")
        code, _, err = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("'newmod'", err)

    def test_new_module_outside_the_cycle_is_fine(self):
        self.update()
        self.write("lib.rs", LIB + "mod newmod;\n")
        self.write("newmod.rs", "use crate::gfx;\n")
        code, out, err = self.run_check()
        self.assertEqual(code, 0, err)


class TestOnlyCode(Fixture):
    def test_edges_inside_cfg_test_blocks_comments_and_strings_are_ignored(self):
        self.update()
        self.append("gfx.rs", textwrap.dedent('''\
            // crate::ui is mentioned in a comment
            /* and in /* a nested */ block comment: crate::app */
            const DOC: &str = "crate::screens";
            const RAW: &str = r#"crate::app "quoted""#;
            #[cfg(test)]
            use crate::app;
            #[cfg(test)]
            mod tests {
                use crate::ui;
                fn t() { let s = "}"; crate::screens::page(); }
            }
            #[cfg(all(test, target_os = "linux"))]
            fn only_test() { crate::app::run(); }
            fn production() { let _c = '}'; let _l: &'static str = "x"; }
        '''))
        code, _, err = self.run_check()
        self.assertEqual(code, 0, err)

    def test_production_code_after_a_test_module_still_counts(self):
        # The old approximation cut everything after the first `#[cfg(test)] mod`.
        self.update()
        self.append("gfx.rs", textwrap.dedent('''\
            #[cfg(test)]
            mod tests { fn t() {} }
            fn after() { crate::ui::draw(); }
        '''))
        code, _, err = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("'gfx'", err)

    def test_cfg_not_test_is_production(self):
        self.update()
        self.append("gfx.rs", "#[cfg(not(test))]\nfn real() { crate::ui::draw(); }\n")
        code, _, err = self.run_check()
        self.assertEqual(code, 1, err)

    def test_files_reached_only_through_cfg_test_mods_are_never_read(self):
        self.update()
        self.append("gfx.rs", '#[cfg(test)]\n#[path = "gfx_tests.rs"]\nmod gfx_tests;\n#[cfg(test)]\nmod tests;\n')
        self.write("gfx_tests.rs", "use crate::app;\n")
        (self.src / "gfx").mkdir()
        self.write("gfx/tests.rs", "use crate::ui;\n")
        code, _, err = self.run_check()
        self.assertEqual(code, 0, err)

    def test_submodule_files_belong_to_their_top_level_module(self):
        self.update()
        self.append("gfx.rs", "mod inner;\n")
        self.write("gfx/inner.rs", "fn f() { crate::app::run(); }\n")
        code, _, err = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("rust-modules/src/gfx/inner.rs:1", err)


class Ratchet(Fixture):
    def make_cycle(self):
        # ui <-> screens is the baseline cycle; app and gfx stay outside.
        self.write("ui.rs", "use crate::{gfx, screens};\n")

    def test_baseline_with_a_cycle_passes_and_records_members(self):
        self.make_cycle()
        snap = self.update()
        self.assertEqual(snap["members"], ["screens", "ui"])
        self.assertEqual(snap["size"], 2)
        self.assertEqual(snap["outside_count"], 3)  # app, gfx, hls
        code, out, err = self.run_check()
        self.assertEqual(code, 0, err)

    def test_shrinking_the_cycle_notices_and_does_not_fail(self):
        self.make_cycle()
        self.update()
        self.write("ui.rs", "use crate::gfx;\n")  # the upward edge is gone
        code, out, err = self.run_check()
        self.assertEqual(code, 0, err)
        self.assertIn("shrank", out)
        self.assertIn("--update-baseline", out)
        self.assertIn("screens", out)

    def test_update_baseline_round_trips_after_shrinking_and_after_growing(self):
        self.make_cycle()
        self.update()
        self.write("ui.rs", "use crate::gfx;\n")
        self.assertEqual(self.update()["members"], [])
        code, out, _ = self.run_check()
        self.assertEqual((code, "shrank" in out), (0, False))
        self.write("gfx.rs", "fn up() { crate::ui::draw(); }\n")
        self.assertEqual(self.run_check()[0], 1)
        self.assertEqual(self.update()["members"], ["gfx", "ui"])
        self.assertEqual(self.run_check()[0], 0)

    def test_a_member_joining_the_existing_cycle_fails(self):
        self.make_cycle()
        self.update()
        self.write("app.rs", "use crate::screens;\n")
        self.write("screens.rs", "use crate::{ui, app};\n")
        code, _, err = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("'app'", err)
        self.assertIn("rust-modules/src/screens.rs:1", err)

    def test_missing_baseline_is_a_failure_with_a_hint(self):
        code, _, err = self.run_check()
        self.assertEqual(code, 1)
        self.assertIn("--update-baseline", err)


class Reports(Fixture):
    def test_report_lists_the_cycle_and_the_thin_back_edge(self):
        self.write("ui.rs", "use crate::{gfx, screens};\n")
        self.write("screens.rs", "use crate::ui;\nuse crate::ui::a;\nuse crate::ui::b;\n")
        code, out, _ = self.run_check("--report")
        self.assertEqual(code, 0)
        self.assertIn("largest cycle (SCC): 2 modules", out)
        self.assertIn("outside it (3): app, gfx, hls", out)
        self.assertIn("ui -> screens", out)
        self.assertIn("rust-modules/src/ui.rs:1", out)

    def test_dot_marks_cycle_members(self):
        self.write("ui.rs", "use crate::{gfx, screens};\n")
        code, out, _ = self.run_check("--dot")
        self.assertEqual(code, 0)
        self.assertIn('"ui" [style=filled', out)
        self.assertIn('"gfx";', out)


class RealTree(unittest.TestCase):
    def test_the_committed_baseline_matches_the_tree(self):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = checker.main([])
        self.assertEqual(code, 0, err.getvalue())
        self.assertNotIn("shrank", out.getvalue(), "the cycle shrank: run ci/check-module-cycle.py --update-baseline")


if __name__ == "__main__":
    unittest.main()
