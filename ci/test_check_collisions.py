#!/usr/bin/env python3
"""Nothing `make check` runs at the same time may collide on a scratch path, a port or the tree.

Two kinds of concurrency exist. Between checkouts: `make check` is bounded to two concurrent runs
machine-wide by tools/check-lock.py, so two worktrees DO run it at once (and `PLX_CHECK_LOCK=off`, a
lone `make check-python-rest` and CI-style single steps are not queued at all). WITHIN one run:
`make check-python-rest` runs ~65 steps a few at a time (tools/check-parallel.py --steps over
ci/check-python-steps.txt), beside the cargo branch and the harness branch, all in one checkout and
one $TMPDIR. The steps below were measured to corrupt each other when two checkouts ran them at once:

  * ci/test-compat.py defaulted `--output` to the fixed `/tmp/plx-compat-tests`
    ("ld: open() failed, errno=17 (File exists) for '.../auxv-tsan'");
  * the C unit tests were compiled to fixed `$(TMPDIR)plx-*-test` binaries.

Each pair test here runs the step in several overlapping pairs at the SAME time against one shared
`TMPDIR` (the shared thing in the real failure), and requires every run to pass and to leave nothing
behind. The within-one-run tests run a mix of DIFFERENT steps together through the real runner against
one shared `TMPDIR` and HOME-less checkout, and require them to pass, to leave nothing in the temp
directory and not to change a tracked file; and they fail on a fixed network port anywhere in the
test sources. No cargo, no network beyond loopback.
"""
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import unittest

import check_steps

ROOT = Path(__file__).resolve().parent.parent
COPIES = 8


def run_pair(cmd, tmpdir):
    """COPIES runs of the step, all started at once, so every two of them overlap (28 overlapping pairs).
    Before PR #458's fix 8 of 12 side-by-side pairs failed; four pairs one after another took 15 s of
    wall, and eight copies at once take ~4 s."""
    env = dict(os.environ, TMPDIR=str(tmpdir))
    procs = [subprocess.Popen(cmd, cwd=ROOT, env=env, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, text=True) for _ in range(COPIES)]
    outs = [p.communicate(timeout=300)[0] for p in procs]
    return [p.returncode for p in procs], outs


class ConcurrentSteps(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='plx-collision-test-')
        self.addCleanup(temp.cleanup)
        self.tmp = Path(temp.name)

    def assert_pairs_pass(self, cmd):
        codes, outs = run_pair(cmd, self.tmp)
        self.assertEqual(codes, [0] * COPIES, '\n---\n'.join(outs))
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


# Distinct steps of ci/check-python-steps.txt that write scratch state in different ways (a C compiler
# into a temp directory, a fake cargo and fake HOME, local TLS and plain sockets, a lock cache), run
# together exactly as the runner runs the manifest. Each must still be in the manifest.
RUN_TOGETHER = (
    'python3 ci/test-compat.py',
    'make --no-print-directory check-c-unit',
    'python3 ci/test_cargo_seed.py',
    'python3 ci/test_tv_ssh.py',
    'python3 ci/test_test_crate.py',
    'python3 ci/test_cargo_test_parallel.py',
    'python3 tools/plxnative-lab selftest',
    'python3 ci/test_libass_fetch.py',
    'python3 ci/test_deploy_manifest.py',
    'python3 tests/test_mock_pms_library.py',
    'python3 tests/test_image_cache_stress.py',
    'python3 ci/test_verify_deploy.py',
)
# A fixed port in a bind or a server constructor. Port 0 (the kernel picks) is what a test may use.
FIXED_PORT = re.compile(r'bind\(\s*\(\s*[\'"][^\'"]*[\'"]\s*,\s*[1-9]'
                        r'|(?:HTTPServer|TCPServer|UDPServer)\(\s*\(\s*[\'"][^\'"]*[\'"]\s*,\s*[1-9]'
                        r'|create_server\(\s*\(\s*[\'"][^\'"]*[\'"]\s*,\s*[1-9]')


class StepsRunTogetherInOneCheck(unittest.TestCase):
    """`make check-python-rest` starts several steps at once in one checkout and one $TMPDIR, beside the
    harness and cargo branches. These pin the ways two DIFFERENT steps could still collide."""

    def test_the_steps_run_together_pass_and_leave_the_temp_dir_and_the_tree_alone(self):
        manifest = set(check_steps.commands())
        self.assertEqual([c for c in RUN_TOGETHER if c not in manifest], [],
                         'a step named here left ci/check-python-steps.txt: update RUN_TOGETHER')
        status = ['git', 'status', '--porcelain', '--untracked-files=all']
        before = subprocess.run(status, cwd=ROOT, capture_output=True, text=True).stdout
        with tempfile.TemporaryDirectory(prefix='plx-steps-together-') as tmp:
            scratch = Path(tmp) / 'tmp'
            scratch.mkdir()
            listing = Path(tmp) / 'steps.txt'
            listing.write_text('\n'.join(RUN_TOGETHER) + '\n')
            env = dict(os.environ, TMPDIR=str(scratch))
            proc = subprocess.run([sys.executable, 'tools/check-parallel.py', '--jobs', str(len(RUN_TOGETHER)),
                                   '--steps', str(listing)], cwd=ROOT, env=env, capture_output=True,
                                  text=True, timeout=300)
            self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
            self.assertEqual(sorted(p.name for p in scratch.iterdir()), [], 'a step left scratch files in $TMPDIR')
        self.assertEqual(subprocess.run(status, cwd=ROOT, capture_output=True, text=True).stdout, before,
                         'a step changed the checkout')

    def test_no_test_source_binds_a_fixed_port(self):
        # Two steps (or this run and another checkout's) binding the same fixed port would make the
        # second fail with "address already in use", and only sometimes. Port 0 is always free.
        hits = []
        for folder in ('ci', 'tools', 'tests', '.claude/hooks'):
            for path in sorted((ROOT / folder).rglob('*')):
                if not path.is_file() or (path.suffix not in ('.py', '.sh') and path.name != 'plxnative-lab'):
                    continue
                for number, line in enumerate(path.read_text(errors='replace').splitlines(), 1):
                    if not line.lstrip().startswith('#') and FIXED_PORT.search(line):
                        hits.append(f'{path.relative_to(ROOT).as_posix()}:{number}: {line.strip()}')
        self.assertEqual(hits, [], 'bind port 0 and read the port back')


if __name__ == '__main__':
    unittest.main()
