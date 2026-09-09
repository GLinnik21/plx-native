#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest
spec = importlib.util.spec_from_file_location('evidence', Path(__file__).with_name('check-link-evidence.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class Inputs(unittest.TestCase):
    def test_own_getauxval_allowed(self):
        m.check_inputs(b'LOAD src/compat/getauxval.o\ngetauxval', b'src/compat/getauxval.o')
    def test_forbidden_archive_and_member_rejected(self):
        for bad in (b'LOAD /sdk/libglibc_polyfills.a', b'LOAD /sdk/libglibc-polyfills.a(getauxval.c.o)', b'LOAD x\n_glibc_polyfills_init'):
            with self.assertRaises(ValueError):
                m.check_inputs(bad, b'trace')
            with self.assertRaises(ValueError):
                m.check_inputs(b'LOAD own.o', bad)
    def test_missing_evidence_rejected(self):
        for pair in [(b'', b'x'), (b'LOAD x', b''), (b'invented summary', b'x')]:
            with self.assertRaises(ValueError):
                m.check_inputs(*pair)

if __name__ == '__main__':
    unittest.main()
