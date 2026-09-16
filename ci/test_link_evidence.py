#!/usr/bin/env python3
import hashlib
import importlib.util
import json
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest
spec = importlib.util.spec_from_file_location('evidence', Path(__file__).with_name('check-link-evidence.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

ROOT = Path(__file__).resolve().parent.parent
STAGE_SCRIPT = Path(__file__).with_name('stage-link-evidence.py')

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


class StageFromHashedCargoOutput(unittest.TestCase):
    """`cargo rustc --bin <name>` links through a HASHED path under
    `target/<triple>/release/deps/<crate>-<hash>` (arm-cc.py's evidence lands
    there) and then cargo COPIES the result to the plain `release/<name>`
    path the Makefile names — a distinct inode, not a symlink. Reproduces
    that shape with no compiler and no NDK: a `deps/` sibling holding the
    real evidence under a hashed name, and a plain-named source file with no
    evidence beside it at all."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='stage-link-evidence-tests-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.release = self.root / 'release'
        self.deps = self.release / 'deps'
        self.deps.mkdir(parents=True)
        self.elf_bytes = b'\x7fELF' + b'synthetic-arm-binary' * 100

    def write_hashed_evidence(self, crate_underscored='plxnative_storage', hashsuf='c485fd1d0c28d026'):
        # Mirrors what cargo/arm-cc.py actually produce: the linker's own `-o` output (the real
        # ELF, same bytes the plain-named copy will carry) sits at the hashed path itself, not
        # only its evidence sidecar files.
        base = self.deps / f'{crate_underscored}-{hashsuf}'
        base.write_bytes(self.elf_bytes)
        link_map = b'LOAD src/main.rs\n'
        link_trace = b'linker invocation trace'
        record = {'schema': 1, 'elf_sha256': hashlib.sha256(self.elf_bytes).hexdigest(),
                   'map_sha256': hashlib.sha256(link_map).hexdigest(),
                   'trace_sha256': hashlib.sha256(link_trace).hexdigest(),
                   'archive_excluded': True}
        Path(str(base) + '.link.map').write_bytes(link_map)
        Path(str(base) + '.link.trace').write_bytes(link_trace)
        Path(str(base) + '.link.json').write_text(json.dumps(record))
        return base

    def run_stage(self, source, destination, extra_args=()):
        return subprocess.run([sys.executable, str(STAGE_SCRIPT), str(source), str(destination), *extra_args],
                               capture_output=True, text=True, timeout=20)

    def test_plain_named_copy_with_no_sidecar_finds_hashed_deps_evidence(self):
        self.write_hashed_evidence()
        source = self.release / 'plxnative-storage'  # cargo's own copy: no `.link.*` beside it.
        source.write_bytes(self.elf_bytes)
        destination = self.root / 'pkg' / 'plxnative-storage'
        destination.parent.mkdir()
        destination.write_bytes(self.elf_bytes)
        result = self.run_stage(source, destination)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.root / 'pkg/plxnative-storage.link.json').is_file())
        self.assertTrue((self.root / 'pkg/plxnative-storage.link.map').is_file())
        record = json.loads((self.root / 'pkg/plxnative-storage.link.json').read_text())
        self.assertEqual(record['linked_elf_sha256'], hashlib.sha256(self.elf_bytes).hexdigest())
        # And the staged evidence must itself pass the real gate.
        m.check_elf(destination, self.root / 'pkg/plxnative-storage')

    def test_never_picks_a_stale_sibling_with_the_wrong_content(self):
        # A hashed sibling exists (from a PRIOR, different build) but its recorded elf_sha256
        # does not match this source's bytes — it must be refused, not silently matched by name.
        base = self.write_hashed_evidence()
        record = json.loads(Path(str(base) + '.link.json').read_text())
        record['elf_sha256'] = '0' * 64  # deliberately wrong
        Path(str(base) + '.link.json').write_text(json.dumps(record))
        source = self.release / 'plxnative-storage'
        source.write_bytes(self.elf_bytes)
        destination = self.root / 'pkg' / 'plxnative-storage'
        destination.parent.mkdir()
        destination.write_bytes(self.elf_bytes)
        result = self.run_stage(source, destination)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('FileNotFoundError', result.stderr)

    def test_real_makefile_recipe_names_the_plain_path_this_fallback_covers(self):
        # Pins the exact invocation shape this regression is about, so a future rewrite of the
        # recipe cannot silently stop exercising the fallback this test proves.
        text = (ROOT / 'Makefile').read_text()
        self.assertIn('python3 ci/stage-link-evidence.py $(STORAGE_BIN) $@', text)


if __name__ == '__main__':
    unittest.main()
