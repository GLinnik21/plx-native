#!/usr/bin/env python3
"""tools/ci-test-results.py on libtest output as the two runners print it: the parallel wrapper's header, cargo's own."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("results", Path(__file__).with_name("ci-test-results.py"))
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)

LOG = """\
   Compiling plx_ui v0.0.0
     Running unittests (target/debug/deps/plx_ui-0123456789abcdef) [plx_ui, 3.2s]

running 5 tests
test focus::tests::moves_right ... ok
test focus::tests::moves_left ... FAILED
test slow::tests::big ... ignored
test slow::tests::big_with_reason ... ignored, takes a minute
test util::sub::deep - should panic ... ok

failures:
    focus::tests::moves_left

test result: FAILED. 2 passed; 1 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running unittests (target/debug/deps/plx_base-fedcba9876543210) [plx_base, 1.0s]

running 2 tests
test focus::tests::moves_right ... ok
test twice ... FAILED
test twice ... ok

test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running unittests src/main.rs (target/debug/deps/plxnative_storage-0123456789abcdef)
test service::tests::starts ... ok
"""


class ParseTest(unittest.TestCase):
    def test_keys_are_package_qualified_and_ignored_tests_do_not_count(self):
        passed, failed = r.parse(LOG)
        self.assertEqual(failed, {"plx_ui::focus::tests::moves_left", "plx_base::twice"})
        self.assertEqual(passed, {"plx_ui::focus::tests::moves_right", "plx_ui::util::sub::deep - should panic",
                                  "plx_base::focus::tests::moves_right", "plxnative_storage::service::tests::starts"})

    def test_a_name_that_passed_and_failed_in_one_log_counts_as_failed_only(self):
        passed, failed = r.parse(LOG)
        self.assertIn("plx_base::twice", failed)
        self.assertNotIn("plx_base::twice", passed)

    def test_lines_that_are_not_test_results_are_ignored(self):
        passed, failed = r.parse("test result: ok. 1 passed\nrunning 1 test\ntestfoo ... ok\n  test a ... ok\nnote: test b ... FAILED\n")
        self.assertEqual((passed, failed), (set(), set()))

    def test_the_document_is_sorted_and_versioned_and_main_writes_it(self):
        with tempfile.TemporaryDirectory() as d:
            log, out = Path(d, "log"), Path(d, "out.json")
            log.write_text(LOG)
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(r.main(["--log", str(log), "--job", "host unit tests", "--out", str(out)]), 0)
            doc = json.loads(out.read_text())
        self.assertEqual((doc["v"], doc["job"]), (1, "host unit tests"))
        self.assertEqual(doc["failed"], sorted(doc["failed"]))
        self.assertEqual(doc["passed"], sorted(doc["passed"]))
        self.assertEqual(len(doc["passed"]) + len(doc["failed"]), 6)


if __name__ == "__main__":
    unittest.main()
