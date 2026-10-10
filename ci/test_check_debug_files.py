#!/usr/bin/env python3
"""ci/check-debug-files.py: `debug-files check` must show symbols, not merely exit 0.

A stub `sentry-cli` prints a canned report per file, so the parsing runs with no network and no
real CLI. The reports follow the shape the release workflow's own comment records (`Arch: arm`,
`Usable: yes`); what only a real CI run can prove is that the pinned CLI still prints this shape,
which is why a report with no feature block fails loudly instead of passing.
"""
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "ci" / "check-debug-files.py"

REPORT = """\
Debug Info File Check
  Type: debug companion
  Contained debug identifiers:
    > Debug ID: 0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0
      Code ID: 0f1e2d3c4b5a69788796a5b4c3d2e1f0aabbccdd
      Arch: arm
      Object: elf
  Contained debug information:
    > %s
  Usable: %s
"""

STUB = """\
#!/usr/bin/env python3
import sys
reports = {reports!r}
print(reports[sys.argv[-1].rsplit("/", 1)[-1]], end="")
sys.exit(int(reports.get("exit:" + sys.argv[-1].rsplit("/", 1)[-1], 0)))
"""


class CheckDebugFiles(unittest.TestCase):
    def run_check(self, reports, names):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            stub = tmp / "fake-sentry-cli"
            stub.write_text(STUB.format(reports=reports))
            stub.chmod(stub.stat().st_mode | stat.S_IXUSR)
            files = []
            for name in names:
                f = tmp / name
                f.write_bytes(b"\x7fELF")
                files.append(str(f))
            env = {**os.environ, "SENTRY_CLI": str(stub)}
            return subprocess.run([sys.executable, "-I", str(SCRIPT), *files], env=env,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def test_symtab_or_debug_passes(self):
        done = self.run_check({"a.debug": REPORT % ("symtab", "yes"),
                               "b.debug": REPORT % ("symtab, debug, unwind", "yes")}, ["a.debug", "b.debug"])
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("a.debug: symtab", done.stdout)
        self.assertIn("b.debug: debug, symtab", done.stdout)

    def test_a_report_with_only_unwind_is_the_stripped_object_and_fails(self):
        done = self.run_check({"s.debug": REPORT % ("unwind", "yes")}, ["s.debug"])
        self.assertEqual(done.returncode, 1)
        self.assertIn("neither debug nor symtab", done.stderr)

    def test_a_feature_reported_as_no_does_not_count(self):
        done = self.run_check({"n.debug": REPORT % ("symtab: no", "yes")}, ["n.debug"])
        self.assertEqual(done.returncode, 1)

    def test_one_bad_file_fails_the_set_and_good_ones_are_still_reported(self):
        done = self.run_check({"g.debug": REPORT % ("symtab", "yes"),
                               "b.debug": REPORT % ("unwind", "yes")}, ["g.debug", "b.debug"])
        self.assertEqual(done.returncode, 1)
        self.assertIn("g.debug: symtab", done.stdout)
        self.assertIn("b.debug", done.stderr)

    def test_unusable_fails_even_with_features(self):
        done = self.run_check({"u.debug": REPORT % ("symtab", "no")}, ["u.debug"])
        self.assertEqual(done.returncode, 1)
        self.assertIn("Usable: no", done.stderr)

    def test_a_nonzero_exit_fails(self):
        done = self.run_check({"x.debug": REPORT % ("symtab", "yes"), "exit:x.debug": 1}, ["x.debug"])
        self.assertEqual(done.returncode, 1)

    def test_an_unrecognised_report_fails_rather_than_passes(self):
        done = self.run_check({"o.debug": "something else entirely\n"}, ["o.debug"])
        self.assertEqual(done.returncode, 1)
        self.assertIn("output format", done.stderr)

    def test_a_missing_or_empty_file_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp) / "e.debug"
            empty.write_bytes(b"")
            done = subprocess.run([sys.executable, "-I", str(SCRIPT), str(empty), str(Path(tmp) / "gone.debug")],
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                  env={**os.environ, "SENTRY_CLI": "false"})
        self.assertEqual(done.returncode, 1)
        self.assertEqual(done.stderr.count("missing or empty"), 2)

    def test_no_arguments_is_a_usage_error(self):
        done = subprocess.run([sys.executable, "-I", str(SCRIPT)], stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True)
        self.assertEqual(done.returncode, 2)


if __name__ == "__main__":
    unittest.main()
