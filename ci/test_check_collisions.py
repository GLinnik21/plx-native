#!/usr/bin/env python3
"""Two `make check` runs on one machine (two worktrees) must not collide on a scratch path.

`make check` is bounded to two concurrent runs machine-wide by tools/check-lock.py, so two checkouts
DO run it at once (and `PLX_CHECK_LOCK=off`, a lone `make check-python-rest` and CI-style single
steps are not queued at all). The steps below were measured to corrupt each other when two checkouts
ran them at once:

  * ci/test-compat.py defaulted `--output` to the fixed `/tmp/plx-compat-tests`
    ("ld: open() failed, errno=17 (File exists) for '.../auxv-tsan'");
  * the C unit tests were compiled to fixed `$(TMPDIR)plx-*-test` binaries.

Each test here runs the step twice at the SAME time against one shared `TMPDIR` (the shared thing
in the real failure) for several rounds, and requires both to pass and to leave nothing behind.
No cargo, no network.
"""
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
ROUNDS = 4


def run_pair(cmd, tmpdir):
    env = dict(os.environ, TMPDIR=str(tmpdir))
    procs = [subprocess.Popen(cmd, cwd=ROOT, env=env, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, text=True) for _ in range(2)]
    outs = [p.communicate(timeout=300)[0] for p in procs]
    return [p.returncode for p in procs], outs


class ConcurrentSteps(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='plx-collision-test-')
        self.addCleanup(temp.cleanup)
        self.tmp = Path(temp.name)

    def assert_pairs_pass(self, cmd):
        for round_no in range(ROUNDS):
            codes, outs = run_pair(cmd, self.tmp)
            self.assertEqual(codes, [0, 0], f'round {round_no + 1}:\n' + '\n---\n'.join(outs))
            self.assertEqual(sorted(p.name for p in self.tmp.iterdir()), [],
                             'a passing run left scratch files behind')

    def test_compat_tests_run_side_by_side(self):
        self.assert_pairs_pass([sys.executable, 'ci/test-compat.py'])

    def test_c_unit_tests_run_side_by_side(self):
        self.assert_pairs_pass(['make', '--no-print-directory', 'check-c-unit'])

    def test_compat_output_flag_keeps_the_results(self):
        out = self.tmp / 'kept'
        subprocess.run([sys.executable, 'ci/test-compat.py', '--output', str(out)],
                       cwd=ROOT, check=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        self.assertTrue((out / 'results.log').is_file())
        self.assertTrue((out / 'auxv-test').is_file())

    def test_no_rust_test_arms_a_trigger_in_the_shared_runtime_root(self):
        # A host test run resolves the runtime root to the bare `/tmp` (only `hostsim` can steer it),
        # so a test that writes `plxnative-<trigger>` there is visible to every other test process
        # on the machine. Two of them did, and a second checkout's player test read the armed
        # `plxnative-failtest` ("left: []  right: [Transport(None)]", 119 of 150 side-by-side runs).
        # Tests arm triggers through `plx_base::devtrig::with_private_triggers` instead. The one
        # production use below is the remote FIFO's path, which is not a test.
        allowed = {'rust-modules/src/remote.rs'}
        hits = []
        for path in (ROOT / 'rust-modules').rglob('*.rs'):
            rel = path.relative_to(ROOT).as_posix()
            if '/target' in rel or rel in allowed:
                continue
            for number, line in enumerate(path.read_text(errors='replace').splitlines(), 1):
                if 'in_runtime_dir("plxnative-' in line:
                    hits.append(f'{rel}:{number}')
        self.assertEqual(hits, [], 'arm triggers with devtrig::with_private_triggers, not in the shared root')

    def test_makefile_names_no_fixed_binary_in_a_shared_temp_dir(self):
        text = (ROOT / 'Makefile').read_text()
        for number, line in enumerate(text.splitlines(), 1):
            if line.lstrip().startswith('#'):
                continue
            self.assertIsNone(re.search(r'-o\s+(\$\(TMPDIR\)|/tmp/|\$\(or \$\(TMPDIR\))', line),
                              f'Makefile:{number} compiles into a fixed shared path: {line.strip()}')
            self.assertNotIn('_TEST_BIN', line, f'Makefile:{number} still uses a fixed test binary')


if __name__ == '__main__':
    unittest.main()
