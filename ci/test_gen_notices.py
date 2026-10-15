#!/usr/bin/env python3
"""ci/gen-third-party-notices.py on synthetic locks and data: the version column follows the lock (a
bumped crate changes the rendered table, two lock versions render "A and B", a std snapshot version is
merged in), a lock package that is neither a row nor `not_shipped` is refused, so is a stale
`not_shipped` entry or a row in no lock, a moved nightly pin is refused, the `compiler_builtins` version in
the prose follows the std snapshot, rendering is idempotent, and the REAL repository's data file and
notices pass the gate (so a hand edit of a generated table, or a bump without `make notices`, fails here
as well as in the gate). Nothing here runs cargo or touches the network."""
import importlib.util
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
        'toolchain': {'pin': 'nightly-2030-01-01', 'rustc': 'rustc 2.0.0-nightly (abc 2030-01-01)', 'backtrace': '9.9.9'},
        'std_lock': {'libc': '0.2.1', 'compiler_builtins': '0.1.5'},
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


PINS = {'nightly-2030-01-01'}


class Render(unittest.TestCase):
    project = {'serde': ['1.0.1'], 'libc': ['0.2.9'], 'icu_x': ['2.0.0'], 'syn': ['2.0.0']}

    def test_versions_come_from_the_locks_and_merge_with_the_std_snapshot(self):
        out = gen.render(notices(), data(), self.project)
        self.assertIn('| serde | 1.0.1 | MIT | D |', out)
        self.assertIn('| libc | 0.2.9 and 0.2.1 | MIT | R |', out)
        self.assertIn('| std | rustc 2.0.0-nightly (abc 2030-01-01) | MIT | Rust |', out)
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

    def test_a_moved_nightly_pin_is_refused(self):
        project = {'serde': ['1.0.1'], 'libc': ['0.2.9'], 'icu_x': ['2.0.0'], 'syn': ['2.0.0']}
        self.assertTrue(any('pin' in p for p in gen.problems_in(data(), project, {'nightly-2030-02-02'})))
        self.assertTrue(any('pin' in p for p in gen.problems_in(data(), project, PINS | {'nightly-2030-02-02'})))


class Repository(unittest.TestCase):
    def test_the_real_notices_match_the_real_lock(self):
        d, project = gen.load()
        self.assertEqual(gen.problems_in(d, project, gen.workflow_pins()), [])
        current = gen.NOTICES.read_text()
        self.assertEqual(gen.render(current, d, project), current,
                         f'THIRD-PARTY-NOTICES.md is stale: run {gen.REGENERATE}')


if __name__ == '__main__':
    unittest.main()
