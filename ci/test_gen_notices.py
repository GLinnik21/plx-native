#!/usr/bin/env python3
"""ci/gen-third-party-notices.py on synthetic locks and data: the version column follows the lock (a
bumped crate changes the rendered table, two lock versions render "A and B", a std snapshot version is
merged in), a lock package that is neither a row nor `not_shipped` is refused, so is a stale
`not_shipped` entry or a row in no lock, a moved toolchain pin is refused, the `compiler_builtins` version in
the prose follows the std snapshot, rendering is idempotent, and the REAL repository's data file and
notices pass the gate (so a hand edit of a generated table, or a bump without `make notices`, fails here
as well as in the gate). Nothing here runs cargo or touches the network."""
import importlib.util
import json
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location('gen_notices', Path(__file__).with_name('gen-third-party-notices.py'))
gen = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gen)


def lock(*pkgs):
    out = ['version = 4', '']
    for name, version, registry in pkgs:
        out += ['[[package]]', f'name = "{name}"', f'version = "{version}"']
        if registry:
            out += ['source = "registry+https://github.com/rust-lang/crates.io-index"']
        out += ['']
    return '\n'.join(out)


def data():
    return {
        'toolchain': {'pin': '2.0.0', 'rustc': 'rustc 2.0.0 (abc 2030-01-01)', 'backtrace': '9.9.9'},
        'std_lock': {'libc': '0.2.1', 'compiler_builtins': '0.1.5'},
        'std_not_shipped': ['rand'],
        'not_shipped': ['syn'],
        'groups': [
            {'id': 'a', 'columns': ['Package', 'Version(s)', 'Declared licence', 'Copyright'],
             'lead': [['std', '{rustc}', 'MIT', 'Rust']],
             'crates': [{'name': 'serde', 'licence': 'MIT', 'copyright': 'D'}, {'name': 'libc', 'licence': 'MIT', 'copyright': 'R'}]},
            {'id': 'b', 'columns': ['Package', 'Version'], 'crates': [{'name': 'icu_x'}]},
        ],
    }


def notices():
    parts = ['intro']
    for gid in ('a', 'b'):
        begin, end = gen.region(gid)
        parts += [begin, '', 'stale', '', end]
    parts += ['**compiler_builtins 0.0.0**, declared MIT']
    return '\n'.join(parts) + '\n'


PINS = {'2.0.0'}


class Render(unittest.TestCase):
    project = {'serde': ['1.0.1'], 'libc': ['0.2.9'], 'icu_x': ['2.0.0'], 'syn': ['2.0.0']}

    def test_versions_come_from_the_locks_and_merge_with_the_std_snapshot(self):
        out = gen.render(notices(), data(), self.project)
        self.assertIn('| serde | 1.0.1 | MIT | D |', out)
        self.assertIn('| libc | 0.2.9 and 0.2.1 | MIT | R |', out)
        self.assertIn('| std | rustc 2.0.0 (abc 2030-01-01) | MIT | Rust |', out)
        self.assertIn('| icu_x | 2.0.0 |', out)
        self.assertIn('**compiler_builtins 0.1.5**', out)
        self.assertNotIn('stale', out)

    def test_a_bump_changes_the_output_and_rendering_is_idempotent(self):
        once = gen.render(notices(), data(), self.project)
        self.assertEqual(once, gen.render(once, data(), self.project))
        bumped = {**self.project, 'serde': ['1.0.2']}
        self.assertIn('| serde | 1.0.2 |', gen.render(once, data(), bumped))
        self.assertNotEqual(once, gen.render(once, data(), bumped))

    def test_rows_sort_by_name_whatever_the_data_order(self):
        out = gen.render(notices(), data(), self.project)
        self.assertLess(out.index('| libc |'), out.index('| serde |'))

    def test_a_missing_region_is_an_error(self):
        with self.assertRaises(SystemExit):
            gen.render('no regions\n**compiler_builtins 1**\n', data(), self.project)


class Classification(unittest.TestCase):
    def test_a_consistent_tree_has_no_problems(self):
        project = gen.parse_lock(lock(('serde', '1.0.1', True), ('libc', '0.2.9', True), ('icu_x', '2.0.0', True),
                                      ('syn', '2.0.0', True), ('plx_ui', '0.0.0', False)))
        self.assertEqual(project['serde'], ['1.0.1'])
        self.assertNotIn('plx_ui', project)  # a workspace member is not a registry package
        self.assertEqual(gen.problems_in(data(), project, PINS), [])

    def test_a_new_lock_package_must_be_classified(self):
        project = {'serde': ['1.0.1'], 'libc': ['0.2.9'], 'icu_x': ['2.0.0'], 'syn': ['2.0.0'], 'newcrate': ['0.1.0']}
        problems = gen.problems_in(data(), project, PINS)
        self.assertEqual(len(problems), 1)
        self.assertIn('newcrate', problems[0])
        self.assertIn('not_shipped', problems[0])

    def test_unclassified_lists_only_the_new_crates(self):
        project = {'serde': ['1.0.1'], 'libc': ['0.2.9'], 'icu_x': ['2.0.0'], 'syn': ['2.0.0'], 'zed': ['1.0.0'], 'abc': ['1.0.0']}
        self.assertEqual(gen.unclassified(data(), project), ['abc', 'zed'])
        project = {'serde': ['9.9.9'], 'libc': ['0.2.9'], 'icu_x': ['2.0.0'], 'syn': ['2.0.0']}  # versions moved only
        self.assertEqual(gen.unclassified(data(), project), [])

    def test_stale_entries_are_refused(self):
        project = {'serde': ['1.0.1'], 'libc': ['0.2.9']}  # icu_x (row) and syn (not_shipped) left the lock
        problems = ' '.join(gen.problems_in(data(), project, PINS))
        self.assertIn('icu_x', problems)
        self.assertIn('syn', problems)

    def test_a_std_only_row_needs_no_project_lock_entry(self):
        d = data()
        d['groups'][0]['crates'].append({'name': 'hashbrown', 'licence': 'MIT', 'copyright': 'A'})
        d['std_lock']['hashbrown'] = '0.1.0'
        project = {'serde': ['1.0.1'], 'libc': ['0.2.9'], 'icu_x': ['2.0.0'], 'syn': ['2.0.0']}
        self.assertEqual(gen.problems_in(d, project, PINS), [])

    def test_a_moved_toolchain_pin_is_refused(self):
        project = {'serde': ['1.0.1'], 'libc': ['0.2.9'], 'icu_x': ['2.0.0'], 'syn': ['2.0.0']}
        self.assertTrue(any('pin' in p for p in gen.problems_in(data(), project, {'2.0.1'})))
        self.assertTrue(any('pin' in p for p in gen.problems_in(data(), project, PINS | {'2.0.1'})))


class Graph(unittest.TestCase):
    """--verify-graph's comparison, on a synthetic `cargo tree` (no cargo is run)."""

    def check(self, tree_text, *pkgs):
        d = data()
        d['not_shipped'] = ['syn', 'once_cell']
        return gen.graph_problems(gen.parse_tree(tree_text), d, gen.parse_lock(lock(*pkgs)))

    BASE = [('serde', '1.0.0', True), ('libc', '0.2.2', True), ('icu_x', '1.0.0', True), ('syn', '2.0.0', True), ('once_cell', '1.0.0', True)]
    TREE = 'plx v0.1.0 (/x)\nserde v1.0.0\nlibc v0.2.2 (*)\nicu_x v1.0.0\n'

    def test_a_consistent_graph_has_no_problems(self):
        self.assertEqual(self.check(self.TREE, *self.BASE), [])

    def test_a_shipping_crate_listed_not_shipped_is_caught(self):
        problems = self.check(self.TREE + 'once_cell v1.0.0\n', *self.BASE)
        self.assertEqual(len(problems), 1)
        self.assertIn('once_cell', problems[0])
        self.assertIn('no row', problems[0])

    def test_a_row_that_left_the_graph_is_caught(self):
        problems = self.check('serde v1.0.0\nlibc v0.2.2\n', *self.BASE)
        self.assertEqual(len(problems), 1)
        self.assertIn('icu_x', problems[0])

    def test_a_version_only_a_build_dependency_uses_is_caught(self):
        problems = self.check(self.TREE, *self.BASE, ('serde', '0.9.0', True))
        self.assertEqual(len(problems), 1)
        self.assertIn("serde ['0.9.0']", problems[0])

    def test_a_std_version_beside_the_project_one_is_fine(self):
        # data(): std_lock libc 0.2.1 is the std workspace's; the tree has only the project's 0.2.2
        self.assertEqual(self.check(self.TREE, *self.BASE), [])


class Modes(unittest.TestCase):
    """The exit codes `.github/workflows/dependabot-notices.yml` branches on, on a throwaway checkout."""

    def checkout(self, lock_pkgs):
        import json, tempfile
        root = Path(tempfile.mkdtemp())
        self.addCleanup(__import__('shutil').rmtree, root)
        (root / 'ci').mkdir()
        (root / 'rust-modules').mkdir()
        (root / '.github' / 'workflows').mkdir(parents=True)
        (root / 'rust-modules' / 'rust-toolchain.toml').write_text('[toolchain]\nchannel = "2.0.0"\n')
        (root / 'ci' / 'third-party-crates.json').write_text(json.dumps(data()))
        (root / 'rust-modules' / 'Cargo.lock').write_text(lock(*lock_pkgs))
        (root / 'THIRD-PARTY-NOTICES.md').write_text(notices())
        return root

    def run_gen(self, root, *flags):
        import io, contextlib
        saved = (gen.ROOT, gen.DATA, gen.NOTICES, gen.LOCK, gen.WORKFLOWS, gen.TOOLCHAIN_FILE)
        self.addCleanup(lambda: [setattr(gen, k, v) for k, v in zip(('ROOT', 'DATA', 'NOTICES', 'LOCK', 'WORKFLOWS', 'TOOLCHAIN_FILE'), saved)])
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = gen.main(['--root', str(root), *flags])
        return code, out.getvalue()

    BASE = (('serde', '1.0.1', True), ('libc', '0.2.9', True), ('icu_x', '2.0.0', True), ('syn', '2.0.0', True))

    def test_a_version_bump_is_rewritten_with_exit_0(self):
        root = self.checkout(self.BASE)
        self.assertEqual(self.run_gen(root)[0], 0)  # first run renders the stale placeholder regions
        (root / 'rust-modules' / 'Cargo.lock').write_text(lock(('serde', '1.0.2', True), *self.BASE[1:]))
        code, _ = self.run_gen(root, '--check')
        self.assertEqual(code, 1)  # stale, and --check never writes
        self.assertIn('| serde | 1.0.1 |', (root / 'THIRD-PARTY-NOTICES.md').read_text())
        self.assertEqual(self.run_gen(root)[0], 0)
        self.assertIn('| serde | 1.0.2 |', (root / 'THIRD-PARTY-NOTICES.md').read_text())

    def test_a_new_crate_exits_2_names_it_and_writes_nothing(self):
        root = self.checkout(self.BASE)
        self.run_gen(root)
        before = (root / 'THIRD-PARTY-NOTICES.md').read_text()
        (root / 'rust-modules' / 'Cargo.lock').write_text(lock(('serde', '1.0.2', True), *self.BASE[1:], ('newcrate', '0.1.0', True)))
        code, out = self.run_gen(root)
        self.assertEqual(code, 2)
        self.assertIn('unclassified: newcrate', out)
        self.assertEqual((root / 'THIRD-PARTY-NOTICES.md').read_text(), before)

    def test_other_inconsistencies_exit_1_not_2(self):
        root = self.checkout(self.BASE[:3])  # `syn` (not_shipped) left the lock: stale entry, nothing new
        code, out = self.run_gen(root)
        self.assertEqual(code, 1)
        self.assertNotIn('unclassified:', out)


class Toolchain(unittest.TestCase):
    """--derive-toolchain / --print-stale-pin: what the Dependabot toolchain path writes, on the REAL data file
    with a faked rust-src (no rustup is run)."""

    def setUp(self):
        self.text = gen.DATA.read_text()
        self.data = json.loads(self.text)
        self.std = {n: [v] for n, v in self.data['std_lock'].items()}
        self.std.update({n: ['1.0.0'] for n in self.data['std_not_shipped']})
        self.real = gen.read_std
        self.addCleanup(setattr, gen, 'read_std', self.real)

    def fake(self, std, rustc='rustc 1.100.0 (abc 2026-11-01)', backtrace='0.3.77'):
        gen.read_std = lambda pin: (std, rustc, backtrace, [])

    def test_the_layout_round_trips_so_a_derive_changes_only_what_moved(self):
        d = self.data
        self.assertEqual(gen.rewrite_toolchain(self.text, d['toolchain']['pin'], d['toolchain']['rustc'], d['toolchain']['backtrace'],
                                               d['std_lock'], d['std_not_shipped']), self.text)

    def test_a_moved_release_rewrites_the_snapshot(self):
        std = {**self.std, 'libc': ['0.2.999'], 'compiler_builtins': ['0.1.999']}
        self.fake(std)
        out, problems, added = gen.derive_toolchain(self.data, self.text, '1.100.0')
        self.assertEqual((problems, added), ([], []))
        new = json.loads(out)
        self.assertEqual(new['toolchain'], {'pin': '1.100.0', 'rustc': 'rustc 1.100.0 (abc 2026-11-01)', 'backtrace': '0.3.77'})
        self.assertEqual(new['std_lock']['libc'], '0.2.999')
        self.assertEqual(new['groups'], self.data['groups'])
        self.assertEqual(new['not_shipped'], self.data['not_shipped'])

    def test_a_crate_new_to_std_needs_a_person_and_nothing_is_rewritten(self):
        self.fake({**self.std, 'brandnew': ['0.1.0']})
        out, problems, added = gen.derive_toolchain(self.data, self.text, '1.100.0')
        self.assertEqual(added, ['brandnew'])
        self.assertEqual(out, self.text)

    def test_a_crate_that_left_std_lock_is_dropped_from_std_not_shipped_but_a_linked_one_needs_a_person(self):
        gone = {k: v for k, v in self.std.items() if k != 'dlmalloc'}
        self.fake(gone)
        out, problems, added = gen.derive_toolchain(self.data, self.text, '1.100.0')
        self.assertEqual((problems, added), ([], []))
        self.assertNotIn('dlmalloc', json.loads(out)['std_not_shipped'])
        self.fake({k: v for k, v in self.std.items() if k != 'libc'})
        out, problems, added = gen.derive_toolchain(self.data, self.text, '1.100.0')
        self.assertTrue(any('libc' in p for p in problems))
        self.assertEqual(out, self.text)

    def test_the_stale_pin_is_a_plain_release_or_an_error(self):
        saved = gen.workflow_pins
        self.addCleanup(setattr, gen, 'workflow_pins', saved)
        gen.workflow_pins = lambda: {self.data['toolchain']['pin']}
        self.assertIsNone(gen.stale_pin(self.data))
        gen.workflow_pins = lambda: {'1.100.0'}
        self.assertEqual(gen.stale_pin(self.data), '1.100.0')
        for bad in ('nightly', '1.100', '1.1.1; rm -rf /', '1.2.3-beta'):
            gen.workflow_pins = lambda bad=bad: {bad}
            with self.assertRaises(SystemExit):
                gen.stale_pin(self.data)

    def test_the_real_std_classification_is_complete_when_the_pinned_toolchain_is_installed(self):
        std, _, _, problems = self.real(self.data['toolchain']['pin'])
        if problems:
            self.skipTest(problems[0])
        self.assertEqual(gen.std_unclassified(std, self.data), [])


class Repository(unittest.TestCase):
    def test_the_real_notices_match_the_real_lock(self):
        d, project = gen.load()
        self.assertEqual(gen.problems_in(d, project, gen.workflow_pins()), [])
        current = gen.NOTICES.read_text()
        self.assertEqual(gen.render(current, d, project), current,
                         f'THIRD-PARTY-NOTICES.md is stale: run {gen.REGENERATE}')


if __name__ == '__main__':
    unittest.main()
