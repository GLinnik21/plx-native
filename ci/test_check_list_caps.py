#!/usr/bin/env python3
"""ci/check-list-caps.py's verdicts on synthetic trees: what is and is not a hit, the registry's
failure modes (unclassed, stale, unknown class, preview without a route, reach without `status =
open`), and the summary line. Each case writes a tiny tree to a private temp dir; nothing here reads
rust-modules/ or the real registry."""
import importlib.util
import tempfile
import unittest
import weakref
from pathlib import Path

_spec = importlib.util.spec_from_file_location('check_list_caps', Path(__file__).with_name('check-list-caps.py'))
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

SCREEN = 'screens/src/shelf.rs'


class Tree:
    """A synthetic rust-modules/: `files` written under a private temp dir, and the registry beside it."""

    def __init__(self, files, ini=''):
        self._temp = tempfile.TemporaryDirectory(prefix='list-caps-')
        weakref.finalize(self, self._temp.cleanup)
        self.modules = Path(self._temp.name).resolve() / 'rust-modules'
        for name, source in files.items():
            path = self.modules / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source)
        self.ini = Path(self._temp.name) / 'list-caps.ini'
        self.ini.write_text(ini)

    def check(self):
        return gate.check(modules=self.modules, ini=self.ini)

    def keys(self):
        return sorted(gate.section_name(*key) for key in self.check()[0])


def entry(key, cls='work', reason='A window.', extra=''):
    return f'[{key}]\nclass = {cls}\nreason = {reason}\n{extra}\n'


class HitShapes(unittest.TestCase):
    def hits(self, body):
        return Tree({SCREEN: body}).keys()

    def test_take_of_a_literal_or_constant_is_a_hit(self):
        self.assertEqual(self.hits('fn a(v: &[u8]) { v.iter().take(8); }'), [f'{SCREEN}::a'])
        self.assertEqual(self.hits('fn a(v: &[u8]) { v.iter().take(SHOWN_ITEMS); }'), [f'{SCREEN}::a'])
        self.assertEqual(self.hits('fn a(v: &[u8]) { v.iter().take(Self::LOTS); }'), [f'{SCREEN}::a'])

    def test_take_of_a_variable_is_not(self):
        self.assertEqual(self.hits('fn a(v: &[u8], n: usize) { v.iter().take(n); v.iter().take(n + 1); }'), [])

    def test_truncate_is_always_a_hit(self):
        self.assertEqual(self.hits('fn a(v: &mut Vec<u8>, n: usize) { v.truncate(n); }'), [f'{SCREEN}::a'])

    def test_min_on_a_length_is_a_hit_but_geometry_is_not(self):
        self.assertEqual(self.hits('fn a(v: &[u8]) -> usize { v.len().min(12) }'), [f'{SCREEN}::a'])
        self.assertEqual(self.hits('fn a(v: &[u8]) -> usize { v.iter().count().min(LIMIT) }'), [f'{SCREEN}::a'])
        self.assertEqual(self.hits('fn a(w: f32) -> f32 { w.min(8) }'), [])
        self.assertEqual(self.hits('fn a(x: i32) -> i32 { x.min(SCR_W) }'), [])

    def test_min_of_a_cap_worded_constant_is_a_hit(self):
        self.assertEqual(self.hits('fn a(n: usize) -> usize { n.min(MAX_CARDS) }'), [f'{SCREEN}::a'])

    def test_cap_worded_const_is_a_hit_at_item_level(self):
        self.assertEqual(self.hits('const MAX_ROWS: usize = 9;'), [f'{SCREEN}::MAX_ROWS'])
        self.assertEqual(self.hits('pub(crate) const PAGE: u32 = 60;'), [f'{SCREEN}::PAGE'])
        self.assertEqual(self.hits('const RESOLVE_LIMIT: i64 = 12;'), [f'{SCREEN}::RESOLVE_LIMIT'])

    def test_an_uncapped_const_name_or_type_is_not(self):
        self.assertEqual(self.hits('const GAP: usize = 9;\nconst MAX_X: f32 = 9.0;\nconst NAME: &str = "MAX";'), [])

    def test_slice_cut_by_a_literal_or_constant_is_a_hit(self):
        self.assertEqual(self.hits('fn a(v: &[u8]) { let _ = &v[..4]; }'), [f'{SCREEN}::a'])
        self.assertEqual(self.hits('fn a(v: &[u8]) { let _ = &v[..HEAD]; }'), [f'{SCREEN}::a'])
        self.assertEqual(self.hits('fn a(v: &[u8], n: usize) { let _ = &v[..n]; let _ = &v[1..]; }'), [])

    def test_fixed_array_with_a_cap_worded_length_is_a_hit(self):
        self.assertEqual(self.hits('struct Row { items: [u32; MAX_ITEMS] }'), [f'{SCREEN}::Row'])
        self.assertEqual(self.hits('struct Row { items: [u32; SLOTS] }'), [])

    def test_comments_and_strings_are_not_code(self):
        self.assertEqual(self.hits('// v.take(8)\nfn a() { let _ = "v.take(8)"; /* v.truncate(3) */ }'), [])

    def test_symbol_is_the_function_not_the_impl_or_closure(self):
        body = 'impl S { fn pick(&self) { self.v.iter().map(|x| x).take(3); } }'
        self.assertEqual(self.hits(body), [f'{SCREEN}::pick'])

    def test_a_use_of_a_registered_constant_is_covered_by_its_definition(self):
        body = ('const MAX_CARDS: usize = 24;\n'
                'fn a(v: &[u8]) { v.iter().take(MAX_CARDS); }\n'
                'fn b(v: &mut Vec<u8>) { v.truncate(MAX_CARDS); }\n')
        self.assertEqual(self.hits(body), [f'{SCREEN}::MAX_CARDS'])

    def test_a_use_of_an_unregistered_constant_is_a_hit(self):
        self.assertEqual(self.hits('fn a(v: &[u8]) { v.iter().take(ELSEWHERE); }'), [f'{SCREEN}::a'])

    def test_a_constant_defined_in_another_file_covers_its_uses_too(self):
        tree = Tree({'data/src/pms.rs': 'pub const MAX_SHELF_ITEMS: usize = 24;',
                     SCREEN: 'fn a(v: &[u8]) { v.iter().take(MAX_SHELF_ITEMS); }'})
        self.assertEqual(tree.keys(), ['data/src/pms.rs::MAX_SHELF_ITEMS'])


class TestCodeIsOutOfScope(unittest.TestCase):
    def test_cfg_test_items_files_and_names_are_skipped(self):
        tree = Tree({
            SCREEN: 'fn real() {}\n#[cfg(test)]\nmod tests { fn t(v: &[u8]) { v.iter().take(3); } }\n',
            'screens/src/shelf_tests.rs': 'fn t(v: &[u8]) { v.iter().take(3); }',
            'screens/src/tests.rs': 'fn t(v: &[u8]) { v.iter().take(3); }',
            'screens/src/tests/deep.rs': 'fn t(v: &[u8]) { v.iter().take(3); }',
            'screens/src/lib.rs': '#[cfg(test)]\nmod only_test;\n',
            'screens/src/only_test.rs': 'fn t(v: &[u8]) { v.iter().take(3); }',
        })
        self.assertEqual(tree.keys(), [])

    def test_the_app_crate_is_scanned_too(self):
        tree = Tree({'src/app/run.rs': 'fn a(v: &[u8]) { v.iter().take(3); }'})
        self.assertEqual(tree.keys(), ['src/app/run.rs::a'])


class Registry(unittest.TestCase):
    BODY = 'fn a(v: &[u8]) { v.iter().take(8); }'
    KEY = f'{SCREEN}::a'

    def problems(self, ini, body=None):
        return Tree({SCREEN: body or self.BODY}, ini).check()[2]

    def test_a_classed_site_passes(self):
        self.assertEqual(self.problems(entry(self.KEY)), [])

    def test_an_unclassed_hit_says_what_to_add(self):
        (problem,) = self.problems('')
        self.assertIn(f'cap site {self.KEY}', problem)
        self.assertIn(f'[{self.KEY}]', problem)
        self.assertIn('class = work | preview | format | not-a-list | reach', problem)

    def test_a_stale_entry_says_to_remove_it(self):
        problems = self.problems(entry(self.KEY) + entry('screens/src/gone.rs::b'))
        self.assertEqual(len(problems), 1)
        self.assertIn('[screens/src/gone.rs::b] matches no cap site', problems[0])
        self.assertIn('remove the section', problems[0])

    def test_an_entry_goes_stale_when_the_cap_is_removed(self):
        problems = self.problems(entry(self.KEY), body='fn a(v: &[u8]) { v.iter(); }')
        self.assertIn('matches no cap site', problems[0])

    def test_an_unknown_class_fails(self):
        (problem,) = self.problems(entry(self.KEY, cls='summary'))
        self.assertIn("class = 'summary' is not one of", problem)

    def test_an_empty_reason_fails(self):
        (problem,) = self.problems(entry(self.KEY, reason=' '))
        self.assertIn('empty reason', problem)

    def test_a_preview_needs_a_route(self):
        (problem,) = self.problems(entry(self.KEY, cls='preview'))
        self.assertIn('preview with no route', problem)
        self.assertEqual(self.problems(entry(self.KEY, cls='preview', extra='route = the heading opens the page')), [])

    def test_route_belongs_to_preview_only(self):
        (problem,) = self.problems(entry(self.KEY, extra='route = somewhere'))
        self.assertIn('has route, which only a preview names', problem)

    def test_reach_needs_status_open(self):
        (problem,) = self.problems(entry(self.KEY, cls='reach'))
        self.assertIn('needs `status = open`', problem)
        self.assertEqual(self.problems(entry(self.KEY, cls='reach', extra='status = open')), [])

    def test_status_belongs_to_reach_only(self):
        (problem,) = self.problems(entry(self.KEY, extra='status = open'))
        self.assertIn('has status, which only a reach entry carries', problem)

    def test_unknown_keys_fail(self):
        (problem,) = self.problems(entry(self.KEY, extra='evidence = x'))
        self.assertIn('unknown key', problem)

    def test_a_malformed_registry_is_one_problem(self):
        (problem,) = Tree({SCREEN: 'fn a() {}'}, 'not an ini').check()[2]
        self.assertIn('does not parse', problem)

    def test_one_entry_covers_every_hit_in_its_symbol(self):
        body = 'fn a(v: &mut Vec<u8>) { v.iter().take(8); v.truncate(4); }'
        tree = Tree({SCREEN: body}, entry(self.KEY))
        sites, _, problems = tree.check()
        self.assertEqual(problems, [])
        self.assertEqual(sites[(SCREEN, 'a')], ['take', 'truncate'])


class Summary(unittest.TestCase):
    def test_summary_counts_every_class_and_shouts_about_open_reach_caps(self):
        files = {SCREEN: ('const MAX_A: usize = 1;\nconst MAX_B: usize = 2;\nconst MAX_C: usize = 3;\n'
                          'const MAX_D: usize = 4;\nconst MAX_E: usize = 5;\n')}
        ini = (entry(f'{SCREEN}::MAX_A') + entry(f'{SCREEN}::MAX_B', cls='preview', extra='route = r')
               + entry(f'{SCREEN}::MAX_C', cls='format') + entry(f'{SCREEN}::MAX_D', cls='not-a-list')
               + entry(f'{SCREEN}::MAX_E', cls='reach', reason='Cuts the cast.', extra='status = open'))
        sites, registry, problems = Tree(files, ini).check()
        self.assertEqual(problems, [])
        lines = gate.summary_lines(sites, registry)
        self.assertEqual(lines[0], f'check-list-caps: REACH CAP (open): {SCREEN}::MAX_E: Cuts the cast.')
        self.assertEqual(lines[-1], 'check-list-caps: 5 sites, every one classed: work 1, preview 1, format 1, '
                                    'not-a-list 1, reach(open) 1')


if __name__ == '__main__':
    unittest.main()
