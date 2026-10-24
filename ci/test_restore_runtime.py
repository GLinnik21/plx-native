#!/usr/bin/env python3
"""The isolated-toolchain contract of ci/restore-source-inputs.py, for the stable prebuilt-std build.

The release builds with rust-modules/rust-toolchain.toml's stable Rust and the target's PREBUILT std, so
a rebuild needs neither `rust-src` nor `-Z build-std`: the sysroot it is given must carry that std and
must not carry rust-src. (Where the bundled rust-src goes, and that the rest of the restore works, is
exercised end to end by test_source_bundle.py.)
"""
import importlib.util
from pathlib import Path
import tempfile
import unittest
spec = importlib.util.spec_from_file_location('restore', Path(__file__).with_name('restore-source-inputs.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
TARGET = 'armv7-unknown-linux-gnueabi'


class PrebuiltStd(unittest.TestCase):
    def test_a_sysroot_with_the_armv7_std_is_accepted(self):
        with tempfile.TemporaryDirectory() as d:
            lib = Path(d) / 'lib/rustlib' / TARGET / 'lib'
            lib.mkdir(parents=True)
            (lib / 'libstd-abc123.rlib').write_bytes(b'x')
            m.require_prebuilt_std(Path(d))

    def test_a_sysroot_without_it_is_refused_and_told_the_fix(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / 'lib/rustlib').mkdir(parents=True)
            with self.assertRaisesRegex(ValueError, 'rustup target add ' + TARGET):
                m.require_prebuilt_std(Path(d))

    def test_the_old_armv6_std_does_not_count(self):
        # arm-unknown-linux-gnueabi's std carries the CP15 barrier that SIGILLs on the TV.
        with tempfile.TemporaryDirectory() as d:
            lib = Path(d) / 'lib/rustlib/arm-unknown-linux-gnueabi/lib'
            lib.mkdir(parents=True)
            (lib / 'libstd-abc123.rlib').write_bytes(b'x')
            with self.assertRaises(ValueError):
                m.require_prebuilt_std(Path(d))


class NoBuildStdPlumbing(unittest.TestCase):
    def test_the_build_std_vendor_merge_is_gone(self):
        self.assertFalse(hasattr(m, 'merge_runtime_vendor'))


if __name__ == '__main__':
    unittest.main()
