#!/usr/bin/env python3
"""ci/check-vfp.py: the scan, the verdict, the flag-list tie, and (when the toolchain is here) the real probe."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import re
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "ci"))
spec = importlib.util.spec_from_file_location("check_vfp", ROOT / "ci" / "check-vfp.py")
vfp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vfp)

VFP_ASM = """plx_vfp_probe_f32:
\tvmov\ts0, r1
\tvmla.f32\ts4, s2, s0
plx_vfp_probe_f64:
\tvmla.f64\td16, d18, d17
"""
SOFT_ASM = """plx_vfp_probe_f32:
\tbl\t__aeabi_fmul
\tbl\t__aeabi_fadd
plx_vfp_probe_f64:
\tbl\t__aeabi_dmul
\tbl\t__aeabi_dadd
"""
NEON_ASM = "\tvmul.f32\tq8, q9, q10\n\tvmul.f32\td16, d17, d18\n"


def make_codegen() -> str:
    return subprocess.run(["make", "-s", "print-rust-codegen"], cwd=ROOT, capture_output=True, text=True).stdout.strip()


class Scan(unittest.TestCase):
    def test_vfp_arithmetic_is_counted_on_s_and_d_registers(self):
        self.assertEqual(vfp.classify(VFP_ASM), {"vfp_f32": 1, "vfp_f64": 1, "soft_calls": 0})

    def test_the_software_helpers_are_counted(self):
        self.assertEqual(vfp.classify(SOFT_ASM), {"vfp_f32": 0, "vfp_f64": 0, "soft_calls": 4})
        self.assertEqual(vfp.classify("\tbl\t__mulsf3\n\tbl\t__adddf3\n")["soft_calls"], 2)

    def test_neon_forms_are_not_mistaken_for_vfp(self):
        # -neon is in the flags; a q-register multiply must never satisfy "uses VFP".
        self.assertEqual(vfp.classify(NEON_ASM)["vfp_f32"], 0)

    def test_a_vfp_load_or_a_call_to_something_else_is_not_a_soft_call(self):
        self.assertEqual(vfp.classify("\tvldr\td16, [r11, #8]\n\tbl\tsome_function\n\tbl\t__aeabi_memcpy\n")["soft_calls"], 0)


class Verdict(unittest.TestCase):
    ON = vfp.classify(VFP_ASM)
    CONTROL = vfp.classify(SOFT_ASM)

    def test_vfp_with_a_live_control_passes(self):
        self.assertEqual(vfp.judge(self.ON, self.CONTROL), [])

    def test_a_rustc_that_stops_honouring_the_flag_fails(self):
        # the flag is silently ignored: the "on" probe now looks like the control
        problems = vfp.judge(self.CONTROL, self.CONTROL)
        self.assertTrue(any("no VFP arithmetic" in p for p in problems), problems)
        self.assertTrue(any("software float helpers" in p for p in problems), problems)

    def test_one_stray_libcall_fails(self):
        on = dict(self.ON, soft_calls=1)
        self.assertTrue(any("software float helpers" in p for p in vfp.judge(on, self.CONTROL)))

    def test_a_dead_control_means_the_pass_proves_nothing(self):
        for label, control in (("compiled to nothing", vfp.classify("")), ("also VFP", self.ON)):
            with self.subTest(label):
                self.assertTrue(any("negative control" in p for p in vfp.judge(self.ON, control)))


class FlagLists(unittest.TestCase):
    def test_config_toml_and_the_makefile_agree(self):
        self.assertEqual(vfp.config_flags(), make_codegen())

    def test_the_codegen_flags_are_the_load_bearing_set(self):
        flags = make_codegen()
        for want in ("-C target-cpu=cortex-a9", "target-feature=-neon,-soft-float", "-C force-frame-pointers=yes"):
            self.assertIn(want, flags)

    def test_the_makefiles_arm_rustflags_start_with_those_flags(self):
        text = (ROOT / "Makefile").read_text()
        self.assertRegex(text, r'(?m)^RUST_ENV = RUSTFLAGS="\$\(RUST_CODEGEN\) ')
        self.assertNotRegex(text, r"RUSTFLAGS=\"-C target-cpu")

    def test_config_flags_parses_a_table_and_refuses_a_missing_one(self):
        good = '[target.armv7-unknown-linux-gnueabi]\nrustflags = ["-C", "a=1", "-C", "b=2"]\n'
        self.assertEqual(vfp.config_flags(good), "-C a=1 -C b=2")
        with self.assertRaises(ValueError):
            vfp.config_flags('[target.other]\nrustflags = ["-C", "a=1"]\n')

    def test_dropping_soft_float_for_the_control(self):
        self.assertEqual(vfp.without_soft_float_off("-C target-cpu=x -C target-feature=-neon,-soft-float -C y"),
                         "-C target-cpu=x -C target-feature=-neon -C y")

    def test_drift_between_the_lists_fails_the_gate(self):
        out = io.StringIO()
        real = vfp.config_flags
        vfp.config_flags = lambda text=None: "-C target-cpu=cortex-a9"
        try:
            with contextlib.redirect_stdout(out):
                rc = vfp.main(["--skip-if-missing"])
        finally:
            vfp.config_flags = real
        self.assertEqual(rc, 1)
        self.assertIn("differ", out.getvalue())

    def test_flags_without_soft_float_fail_before_compiling(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = vfp.main(["--rustflags", "-C target-cpu=cortex-a9 -C target-feature=-neon"])
        self.assertEqual(rc, 1)
        self.assertIn("-soft-float", out.getvalue())


class LiveProbe(unittest.TestCase):
    """The real compile, when the pinned toolchain and its armv7 target are installed (CI's host jobs install them)."""

    def run_main(self, *args):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = vfp.main(list(args))
        return rc, out.getvalue()

    def test_the_makefiles_flags_give_vfp_and_the_control_gives_libcalls(self):
        rc, text = self.run_main("--skip-if-missing")
        if "SKIP" in text:
            self.skipTest(text.strip())
        self.assertEqual(rc, 0, text)
        self.assertRegex(text, r"0 soft-float calls")
        self.assertRegex(text, r"control without -soft-float: [2-9]\d* libcalls|control without -soft-float: [1-9]\d+ libcalls")

    def test_a_missing_toolchain_is_a_skip_only_when_asked(self):
        rc, text = self.run_main("--toolchain", "0.0.1-not-installed", "--skip-if-missing")
        self.assertEqual((rc, "SKIP" in text), (0, True), text)
        rc, text = self.run_main("--toolchain", "0.0.1-not-installed")
        self.assertEqual(rc, 1, text)
        self.assertIn("rustup toolchain install", text)


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)
